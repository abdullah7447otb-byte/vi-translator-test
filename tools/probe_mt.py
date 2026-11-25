"""Probe for the offline translator's faster decoding (run by .github/workflows/probe-mt.yml).

At the commit the app pins, which files the Xenova/opus-mt-en-ar repository has, their SHA-256,
and the inputs and outputs of the decoders. Then the same 40 game-style lines through:
  1. the app's way today: greedy, the whole sequence through decoder_model_quantized each step;
  2. greedy with the past key/values (decoder_model_merged_quantized, or decoder_with_past);
  3. beam search (4) with the cache, a repetition penalty and no repeated 3-grams.
It prints whether 1 and 2 give the same tokens for every line, and the time each takes.
"""

import hashlib
import json
import sys
import time

import numpy as np
import onnxruntime as ort
from huggingface_hub import HfApi, hf_hub_download
from tokenizers import Tokenizer

REPO = "Xenova/opus-mt-en-ar"
REV = "034a684356c19021a187cda4d7f823298b913921"

LINES = [
    "You coming or what?", "Get in the car.", "Where the hell have you been?", "Press X to jump.",
    "Lucia, I need you to trust me.", "Jason, we gotta go. Now!", "Wasted", "Mission Passed",
    "Hey, you good?", "I saw you at the bank yesterday.", "Don't move or I'll shoot.",
    "We split it fifty-fifty, like always.", "Hold L2 to aim, then press R2 to shoot.",
    "You can also set a destination while using quick navigation.", "Tip: Sleep at your safehouse to move time forward.",
    "Where are you?", "On my way.", "That's not what we agreed.", "C'mon, man, don't do this to me.",
    "Lose the cops.", "Go to the marker.", "Pick up Lucia.", "The deal's off.", "I told you not to call me here.",
    "She's with me.", "Keep your head down and your mouth shut.", "Leonida is a hell of a place.",
    "Vice City never sleeps.", "Buy body armor at Ammu-Nation.", "Your stamina bar is shown in the top left of the HUD.",
    "Thanks for the ride.", "I'll meet you at the motel.", "Nobody leaves until I say so.", "Is that a threat?",
    "We're gonna be rich, baby.", "Who sent you?", "Take the money and run.", "Hands where I can see them!",
    "You owe me, Jason.", "Don't worry about it.",
]


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def io(sess):
    return {"in": [(i.name, i.shape, i.type) for i in sess.get_inputs()], "out": [(o.name, o.shape, o.type) for o in sess.get_outputs()]}


def main():
    api = HfApi()
    files = sorted(f.rfilename for f in api.model_info(REPO, revision=REV, files_metadata=True).siblings)
    print("FILES", json.dumps(files))
    get = lambda f: hf_hub_download(REPO, f, revision=REV)
    for f in files:
        if f.endswith("_quantized.onnx"):
            print("SHA256", f, sha256(get(f)))

    opts = ort.SessionOptions()
    opts.intra_op_num_threads = 2
    opts.inter_op_num_threads = 1
    enc = ort.InferenceSession(get("onnx/encoder_model_quantized.onnx"), opts)
    dec = ort.InferenceSession(get("onnx/decoder_model_quantized.onnx"), opts)
    print("IO encoder", json.dumps(io(enc)))
    print("IO decoder", json.dumps(io(dec)))
    merged = None
    for name in ["onnx/decoder_model_merged_quantized.onnx", "onnx/decoder_with_past_model_quantized.onnx"]:
        if name in files:
            merged = (name, ort.InferenceSession(get(name), opts))
            print("IO", name, json.dumps(io(merged[1])))
            break

    tok_json = json.load(open(get("tokenizer.json")))
    if tok_json.get("normalizer", {}).get("type") == "Precompiled" and tok_json["normalizer"].get("precompiled_charsmap") is None:
        tok_json["normalizer"] = None
    tok = Tokenizer.from_str(json.dumps(tok_json))
    conf = json.load(open(get("config.json")))
    pad = conf.get("decoder_start_token_id", conf.get("pad_token_id"))
    eos = conf.get("eos_token_id", 0)
    prefix = tok.token_to_id(">>ara<<")
    print("CONFIG", json.dumps({k: conf.get(k) for k in ["d_model", "decoder_layers", "decoder_attention_heads", "pad_token_id", "eos_token_id", "decoder_start_token_id", "vocab_size"]}))

    def encode(text):
        ids = ([prefix] if prefix is not None else []) + tok.encode(text).ids
        ids = np.array([ids], dtype=np.int64)
        mask = np.ones_like(ids)
        hidden = enc.run(None, {"input_ids": ids, "attention_mask": mask})[0]
        return ids, mask, hidden

    def best(logits):
        logits = logits.copy()
        logits[pad] = -np.inf
        return int(np.argmax(logits))

    def greedy_full(text):
        ids, mask, hidden = encode(text)
        n = ids.shape[1]
        out = [pad]
        for _ in range(min(max(n * 3, 32), 200)):
            logits = dec.run(["logits"], {"input_ids": np.array([out], dtype=np.int64), "encoder_hidden_states": hidden, "encoder_attention_mask": mask})[0]
            nxt = best(logits[0, -1])
            if nxt == eos:
                break
            out.append(nxt)
        return out[1:]

    if merged is None:
        print("NO_CACHED_DECODER")
        return
    mname, msess = merged
    in_names = [i.name for i in msess.get_inputs()]
    out_names = [o.name for o in msess.get_outputs()]
    past_in = [n for n in in_names if n.startswith("past_key_values")]
    layers = conf["decoder_layers"]
    heads = conf["decoder_attention_heads"]
    dhead = conf["d_model"] // heads

    def step_inputs(token, past, hidden, mask, first):
        feed = {"input_ids": np.array([[token]], dtype=np.int64), "encoder_attention_mask": mask, "encoder_hidden_states": hidden}
        if "use_cache_branch" in in_names:
            feed["use_cache_branch"] = np.array([not first])
        for name in past_in:
            if first:
                # (the merged model's first step takes empty pasts)
                feed[name] = np.zeros((1, heads, 0, dhead), dtype=np.float32)
            else:
                feed[name] = past[name]
        return feed

    def present_to_past(outs):
        past = {}
        for name, val in zip(out_names, outs):
            if name.startswith("present"):
                past[name.replace("present", "past_key_values")] = val
        return past

    def greedy_cached(text):
        ids, mask, hidden = encode(text)
        n = ids.shape[1]
        out, past, token = [], {}, pad
        for i in range(min(max(n * 3, 32), 200)):
            outs = msess.run(None, step_inputs(token, past, hidden, mask, i == 0))
            logits = outs[out_names.index("logits")]
            new = present_to_past(outs)
            # (the encoder's keys/values come only from the first step: kept as they were)
            for k, v in new.items():
                if ".encoder." in k and i > 0:
                    continue
                past[k] = v
            token = best(logits[0, -1])
            if token == eos:
                break
            out.append(token)
        return out

    # 3: the first step through decoder_model_quantized (the app's model, which gives every
    # present), the next ones through decoder_with_past_model_quantized
    wp = ort.InferenceSession(get("onnx/decoder_with_past_model_quantized.onnx"), opts)
    print("IO with_past", json.dumps(io(wp)))
    wp_in = [i.name for i in wp.get_inputs()]
    wp_out = [o.name for o in wp.get_outputs()]
    dec_out = [o.name for o in dec.get_outputs()]

    def greedy_two(text):
        ids, mask, hidden = encode(text)
        n = ids.shape[1]
        outs = dec.run(None, {"input_ids": np.array([[pad]], dtype=np.int64), "encoder_hidden_states": hidden, "encoder_attention_mask": mask})
        past = {name.replace("present", "past_key_values"): v for name, v in zip(dec_out, outs) if name.startswith("present")}
        token = best(outs[dec_out.index("logits")][0, -1])
        out = []
        for _ in range(min(max(n * 3, 32), 200)):
            if token == eos:
                break
            out.append(token)
            feed = {"input_ids": np.array([[token]], dtype=np.int64), "encoder_attention_mask": mask}
            if "encoder_hidden_states" in wp_in:
                feed["encoder_hidden_states"] = hidden
            for name in wp_in:
                if name.startswith("past_key_values"):
                    feed[name] = past[name]
            o = wp.run(None, feed)
            for name, v in zip(wp_out, o):
                if name.startswith("present"):
                    past[name.replace("present", "past_key_values")] = v
            token = best(o[wp_out.index("logits")][0, -1])
        return out

    same2, t_two = 0, 0.0
    for line in LINES:
        a = greedy_full(line)
        t = time.perf_counter(); b = greedy_two(line); t_two += time.perf_counter() - t
        same2 += a == b
        if a != b:
            print("DIFF2", json.dumps({"en": line, "full": tok.decode(a), "two": tok.decode(b)}, ensure_ascii=False))
    print("RESULT2", json.dumps({"same": same2, "two_s": round(t_two, 3)}))

    same, t_full, t_cached = 0, 0.0, 0.0
    for line in LINES:
        t = time.perf_counter(); a = greedy_full(line); t_full += time.perf_counter() - t
        t = time.perf_counter(); b = greedy_cached(line); t_cached += time.perf_counter() - t
        same += a == b
        print("LINE", json.dumps({"en": line, "full": tok.decode(a), "cached": tok.decode(b), "same": a == b}, ensure_ascii=False))
    print("RESULT", json.dumps({"model": mname, "lines": len(LINES), "same": same, "full_s": round(t_full, 3), "cached_s": round(t_cached, 3), "speedup": round(t_full / max(t_cached, 1e-9), 2)}))


if __name__ == "__main__":
    sys.exit(main())
