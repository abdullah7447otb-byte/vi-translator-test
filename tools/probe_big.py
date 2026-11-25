"""Probe: is there an ONNX export of Helsinki-NLP/opus-mt-tc-big-en-ar, and how does it translate and
how fast (greedy, the whole sequence each step, 2 threads, as the app does today)?"""

import json
import time

import numpy as np
import onnxruntime as ort
from huggingface_hub import HfApi, hf_hub_download
from tokenizers import Tokenizer

from probe_mt import LINES, sha256

api = HfApi()
found = [m.id for m in api.list_models(search="opus-mt-tc-big-en-ar", limit=50)]
print("MODELS", json.dumps(found))
for repo in found:
    try:
        info = api.model_info(repo, files_metadata=True)
    except Exception as e:
        print("SKIP", repo, e)
        continue
    files = sorted(f.rfilename for f in info.siblings)
    print("FILES", repo, info.sha, json.dumps(files))
    enc_f = next((f for f in files if f.endswith("encoder_model_quantized.onnx") or f.endswith("encoder_model_int8.onnx")), None)
    dec_f = next((f for f in files if f.endswith("/decoder_model_quantized.onnx") or f.endswith("/decoder_model_int8.onnx")), None)
    if not (enc_f and dec_f and "tokenizer.json" in files):
        continue
    get = lambda f: hf_hub_download(repo, f, revision=info.sha)
    for f in (enc_f, dec_f):
        print("SHA256", repo, f, sha256(get(f)))
    opts = ort.SessionOptions()
    opts.intra_op_num_threads = 2
    opts.inter_op_num_threads = 1
    enc = ort.InferenceSession(get(enc_f), opts)
    dec = ort.InferenceSession(get(dec_f), opts)
    tj = json.load(open(get("tokenizer.json")))
    if tj.get("normalizer", {}).get("type") == "Precompiled" and tj["normalizer"].get("precompiled_charsmap") is None:
        tj["normalizer"] = None
    tok = Tokenizer.from_str(json.dumps(tj))
    conf = json.load(open(get("config.json")))
    pad = conf.get("decoder_start_token_id", conf.get("pad_token_id"))
    eos = conf.get("eos_token_id", 0)
    prefix = tok.token_to_id(">>ara<<")
    total = 0.0
    for line in LINES:
        t = time.perf_counter()
        ids = np.array([([prefix] if prefix is not None else []) + tok.encode(line).ids], dtype=np.int64)
        mask = np.ones_like(ids)
        hidden = enc.run(None, {"input_ids": ids, "attention_mask": mask})[0]
        out = [pad]
        for _ in range(min(max(ids.shape[1] * 3, 32), 200)):
            logits = dec.run(["logits"], {"input_ids": np.array([out], dtype=np.int64), "encoder_hidden_states": hidden, "encoder_attention_mask": mask})[0][0, -1].copy()
            logits[pad] = -np.inf
            nxt = int(np.argmax(logits))
            if nxt == eos:
                break
            out.append(nxt)
        total += time.perf_counter() - t
        print("BIG", json.dumps({"repo": repo, "en": line, "ar": tok.decode(out[1:])}, ensure_ascii=False))
    print("BIGRESULT", json.dumps({"repo": repo, "lines": len(LINES), "seconds": round(total, 3)}))
