//! English → target-language translation engines (Arabic and the other languages in `langs`).
//!
//! - local:  OPUS-MT en→ar (Helsinki-NLP, ONNX export by Xenova) run on the CPU with ONNX Runtime. Free, offline.
//! - deepl / azure / google / libre: their REST translation APIs (free tiers available).
//! - claude: the Claude Messages API (raw HTTP; there is no official Rust SDK).
//! - openai: ChatGPT, the OpenAI Responses API (raw HTTP). Same prompt and tags as claude.
//!
//! Output is a list of segments `(text, colour)` so highlighted English words can keep their colour.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
#[cfg(feature = "offline")]
use ort::session::Session;
#[cfg(feature = "offline")]
use ort::value::Tensor;
use serde_json::{Value, json};
#[cfg(feature = "offline")]
use tokenizers::Tokenizer;

use crate::config::Config;

/// Apple's translation failing again and again: the offline translator for a while (apple_translate::Breaker).
static APPLE_BREAKER: std::sync::Mutex<crate::apple_translate::Breaker> = std::sync::Mutex::new(crate::apple_translate::Breaker::new());

pub type Segments = Vec<(String, Option<String>)>;

// ------------------------------------------------------------------------------------- local model

#[cfg(feature = "offline")]
const MODEL_FILES: [&str; 4] =
    ["onnx/encoder_model_quantized.onnx", "onnx/decoder_model_quantized.onnx", "tokenizer.json", "config.json"];

/// Each offline model at a fixed version (its commit), and the SHA-256 of its two model files: a
/// download is what was tested, or it isn't used. (The tokenizer and config are small text files,
/// fixed by the commit.) From the Hugging Face API; every language with a `local` model has one.
#[cfg(feature = "offline")]
const PINNED: [(&str, &str, [&str; 2]); 8] = [
    ("Xenova/opus-mt-en-ar", "034a684356c19021a187cda4d7f823298b913921", ["53c354d87ced44b39085fe8a46a25f09ea4578d442dbce9663f3977228f8a04e", "94e6bdf90866151787d014b7b77be0adb0f036418ee04d235ec01238aa8426b9"]),
    ("Xenova/opus-mt-en-hi", "7aad72006a1588a9c3485ed6203aed99eebbc872", ["3045cd6eaf06db96cff853204d6f0ffeb0c5af238a4f5a87a19fcaa94f7493c4", "f563f658721046b17aa3f3ec33d12efdb1497c962876ff7d7a5ca513f6b49f4e"]),
    ("Xenova/opus-mt-en-id", "b3b41f654c2fb3286d007dee1da7b0f47fd27d82", ["9bfc9cf0f400f86ec11da24b2f56380fcfabab17fb2dbcf7bb4c1754b7076e24", "d46dea5d13bec5ed7552baa5c0917e0d3dfa32e14fb7f09eae534c50d0cfb42f"]),
    ("Xenova/opus-mt-en-vi", "30bcd461c0d7338ad106b36314d3eedc2d4e998c", ["dfcd7a32be3fdd21cbcf5d354520d626f1768fd6e205ca62a6a11a4641d31f36", "cde3e02aaca72cfaf2267f6c31c4dcc1b323d769bf90c273f2a1c1d56198ddba"]),
    ("Xenova/opus-mt-en-uk", "e6ca587292984b343fd829b6430ed55858496525", ["234c3738c258b3b7c913452a4ab6203b1a24f459355af8ef563dd4d94792d087", "60881867e00ee1c5ef5fb79cde3a52e5d76437199731cd6896b1cd948bb3ea29"]),
    ("Xenova/opus-mt-en-nl", "c3a0541231a7329b4492809fc2bd7ab7d2ceb0bc", ["0febb990f8ca8ed9fd3e0017dfcb55ec5293999f20ead81d2bb4370e9e3120ff", "e9d1474106c1bdf8aaf236eb7ace8a29edf6fee655c583132f521e5b63ccbf97"]),
    ("Xenova/opus-mt-en-ro", "3d816891778276930df2a45cbfb70689b3fe8981", ["843d42689aeba847c71e88863d9d6fbba43cac21d115f375e90afc9922ceadc6", "750ae52709f955bc95aec9cf7a4ea3d9e5525fc164e58b36a3a6424bfafd921f"]),
    ("Xenova/opus-mt-en-cs", "ed163ca109a93297e2f055eb3397bd33230141ad", ["5ca546c92abb310fc14e5dcfc421252a83b1595ca91d117cac9afda7ab7d7c38", "d6b2a757e26d10cc8f1a290174ab6655c623eeb11bdf71bdf44429f30a1cc164"]),
];

#[cfg(feature = "offline")]
/// The SHA-256 of a file, in hex.
fn file_sha256(path: &Path) -> Result<String> {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    std::io::copy(&mut std::fs::File::open(path)?, &mut h)?;
    Ok(format!("{:x}", h.finalize()))
}

#[cfg(feature = "offline")]
/// Download one model file at the pinned commit into `path` (through `path.part`, which a cut-off
/// download goes on from next time), checking its SHA-256 when it has one.
fn download_model_file(http: &reqwest::blocking::Client, repo: &str, rev: &str, f: &str, path: &Path, sha: Option<&str>) -> Result<()> {
    use std::io::Write as _;
    let part = path.with_extension("part");
    let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let mut req = http.get(format!("https://huggingface.co/{repo}/resolve/{rev}/{f}")).timeout(Duration::from_secs(600));
    if have > 0 && sha.is_some() {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut resp = req.send().with_context(|| format!("download {f}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        // (the part is whole already: checked below)
    } else {
        let mut resp_ok = resp.error_for_status_ref().map(|_| ()).with_context(|| format!("download {f}"));
        if resp_ok.is_ok() {
            // 206: the rest, after what's there; 200: the whole file again
            let append = status == reqwest::StatusCode::PARTIAL_CONTENT;
            let mut file = std::fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(&part)?;
            resp_ok = resp.copy_to(&mut file).map(|_| ()).with_context(|| format!("download {f}"));
            file.flush()?;
            file.sync_all()?;
        }
        resp_ok?;
    }
    if let Some(want) = sha {
        if file_sha256(&part)? != want {
            let _ = std::fs::remove_file(&part);
            bail!("download {f}: the file isn't the one expected (it will be downloaded again)");
        }
    }
    std::fs::rename(&part, path)?;
    Ok(())
}

#[cfg(feature = "offline")]
/// ONNX Runtime allows one environment per process; create it once and keep it for the app's lifetime.
fn ort_env() -> Result<&'static ort::environment::Environment> {
    static ENV: std::sync::OnceLock<ort::environment::Environment> = std::sync::OnceLock::new();
    if let Some(e) = ENV.get() {
        return Ok(e);
    }
    let env = ort::init().with_name("vi-translator").build()?;
    Ok(ENV.get_or_init(|| env))
}

#[cfg(feature = "offline")]
pub struct LocalMt {
    tok: Tokenizer,
    /// target-language token some models need first (">>ara<<", ">>vie<<")
    prefix_id: Option<u32>,
    /// padding = the decoder's start token; end of sentence
    pad_id: i64,
    eos_id: i64,
    enc: Session,
    dec: Session,
}

#[cfg(feature = "offline")]
impl LocalMt {
    /// Loads the model for `lang`, downloading it first (~110 MB, once) if it is missing.
    pub fn load(dir: &Path, lang: &crate::langs::Lang, http: &reqwest::blocking::Client, status: &dyn Fn(String), threads: usize) -> Result<Self> {
        let repo = lang.local.ok_or_else(|| {
            anyhow!("There is no free offline model for {}: choose another translation service in the Translator tab.", lang.name)
        })?;
        let (rev, sums) = PINNED.iter().find(|p| p.0 == repo).map(|p| (p.1, p.2)).ok_or_else(|| anyhow!("no fixed version of {repo}"))?;
        for (i, f) in MODEL_FILES.iter().enumerate() {
            let path = dir.join(f);
            // (files already here were downloaded before: used as they are)
            if path.exists() {
                continue;
            }
            status(format!("Downloading the free translation model: {f} ..."));
            std::fs::create_dir_all(path.parent().unwrap())?;
            download_model_file(http, repo, rev, f, &path, sums.get(i).copied())?;
        }
        status("Loading the translation model ...".into());
        // This export has an empty "Precompiled" normalizer, which the Rust tokenizer rejects; drop it.
        let mut json: Value = serde_json::from_slice(&std::fs::read(dir.join("tokenizer.json"))?)?;
        if json["normalizer"]["type"] == "Precompiled" && json["normalizer"]["precompiled_charsmap"].is_null() {
            json["normalizer"] = Value::Null;
        }
        let tok = Tokenizer::from_bytes(serde_json::to_vec(&json)?).map_err(|e| anyhow!("tokenizer: {e}"))?;
        // multi-language models need their target token first (en→ar: ">>ara<<")
        let prefix_id = [">>ara<<", ">>vie<<"].iter().find_map(|t| tok.token_to_id(t));
        let conf: Value = serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)?;
        let pad_id = conf["decoder_start_token_id"].as_i64().or(conf["pad_token_id"].as_i64()).context("model config: no start token")?;
        let eos_id = conf["eos_token_id"].as_i64().unwrap_or(0);
        let env = ort_env()?;
        // (a few threads, not all of the computer's: the picture, its decoding and the reading run
        // beside it, and a line is short)
        let session = |f: &str| -> Result<Session> {
            Ok(Session::builder(env)?
                .with_intra_threads(threads)
                .map_err(|e| anyhow!("translation model: {e}"))?
                .with_inter_threads(1)
                .map_err(|e| anyhow!("translation model: {e}"))?
                .commit_from_file(dir.join(f))?)
        };
        let enc = session(MODEL_FILES[0])?;
        let dec = session(MODEL_FILES[1])?;
        Ok(Self { tok, prefix_id, pad_id, eos_id, enc, dec })
    }

    pub fn translate(&mut self, text: &str) -> Result<String> {
        // (the target-language token, when the model needs one, is added by id so it isn't split)
        let encoding = self.tok.encode(text, true).map_err(|e| anyhow!("{e}"))?;
        let ids: Vec<i64> =
            self.prefix_id.map(|p| p as i64).into_iter().chain(encoding.get_ids().iter().map(|&i| i as i64)).collect();
        let n = ids.len();
        let mask = vec![1i64; n];

        let (d, hidden) = {
            let out = self.enc.run(ort::inputs![
                "input_ids" => Tensor::from_array(([1usize, n], ids))?,
                "attention_mask" => Tensor::from_array(([1usize, n], mask.clone()))?,
            ])?;
            let (shape, data) = out["last_hidden_state"].try_extract_tensor::<f32>()?;
            (shape[2] as usize, data.to_vec())
        };

        // Greedy decoding (no KV cache; subtitle lines are short).
        let (pad, eos) = (self.pad_id, self.eos_id);
        let mut out_ids = vec![pad];
        for _ in 0..(n * 3).max(32).min(200) {
            let m = out_ids.len();
            let next = {
                let out = self.dec.run(ort::inputs![
                    "input_ids" => Tensor::from_array(([1usize, m], out_ids.clone()))?,
                    "encoder_hidden_states" => Tensor::from_array(([1usize, n, d], hidden.clone()))?,
                    "encoder_attention_mask" => Tensor::from_array(([1usize, n], mask.clone()))?,
                ])?;
                let (shape, logits) = out["logits"].try_extract_tensor::<f32>()?;
                let v = shape[2] as usize;
                let last = &logits[(m - 1) * v..m * v];
                let mut best = (0usize, f32::MIN);
                for (i, &l) in last.iter().enumerate() {
                    if i as i64 != pad && l > best.1 {
                        best = (i, l);
                    }
                }
                best.0 as i64
            };
            if next == eos {
                break;
            }
            out_ids.push(next);
        }
        let ids: Vec<u32> = out_ids[1..].iter().map(|&i| i as u32).collect();
        Ok(self.tok.decode(&ids, true).map_err(|e| anyhow!("{e}"))?.trim().to_string())
    }
}

/// Whether this build has the free offline translator (the phone apps and the Intel Mac app don't:
/// ONNX Runtime is big, and has no Intel Mac build). Without it "local" means the reviewed
/// translations only.
pub const HAS_OFFLINE: bool = cfg!(feature = "offline");

/// This build has no offline translator (the phone apps leave it out: the ONNX Runtime it needs is
/// big). Choosing "local" says so.
#[cfg(not(feature = "offline"))]
pub struct LocalMt;

#[cfg(not(feature = "offline"))]
impl LocalMt {
    pub fn load(_dir: &Path, _lang: &crate::langs::Lang, _http: &reqwest::blocking::Client, _status: &dyn Fn(String), _threads: usize) -> Result<Self> {
        bail!("The free offline translator is not part of this app: choose another translation service.")
    }

    pub fn translate(&mut self, _text: &str) -> Result<String> {
        bail!("no offline translator in this build")
    }
}

/// True when a translation into a language not written in Latin letters (`native`: its own name)
/// has two or more English words the English line didn't have: an answer in English, not a translation.
/// Names and terms kept from the line (GTA+, Vice City) are in it, so they don't count.
fn english_leak(line: &str, out: &str, native: &str) -> bool {
    if native.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let words = |t: &str| -> Vec<String> {
        t.split(|c: char| !c.is_ascii_alphabetic()).filter(|w| w.len() >= 2).map(|w| w.to_ascii_lowercase()).collect()
    };
    let have: std::collections::HashSet<String> = words(line).into_iter().collect();
    words(out).iter().filter(|w| !have.contains(*w)).count() >= 2
}

/// True when a translation has far more letters than the line (3× and 12 more): made up, not translated.
/// A translation runs about as long as its English, give or take; names kept as they are count alike.
fn made_up(line: &str, out: &str) -> bool {
    let letters = |t: &str| t.chars().filter(|c| c.is_alphanumeric()).count();
    letters(out) > 3 * letters(line) + 12
}

// ------------------------------------------------------------------------------------- translator

/// GTA VI's two leads, for the language models' grammar (who is a man, who is a woman): only their
/// names and that, as the game's makers have shown them.
const GTA6_CAST: &str = "\n\
             - The game is Grand Theft Auto VI. Its two lead characters: Jason (Jason Duval), a man, and\n\
               Lucia (Lucia Caminos), a woman.";

const DIALECTS: [(&str, &str); 4] = [
    ("msa", "Modern Standard Arabic (الفصحى), natural and readable, not stiff"),
    ("egyptian", "Egyptian Arabic dialect (اللهجة المصرية)"),
    ("gulf", "Gulf Arabic dialect (اللهجة الخليجية)"),
    ("levantine", "Levantine Arabic dialect (اللهجة الشامية)"),
];

/// USD per million tokens (input, output) for the cost counter.
fn claude_price(model: &str) -> (f64, f64) {
    match model {
        m if m.starts_with("claude-haiku-5-5") => (0.10, 0.50),
        m if m.starts_with("claude-sonnet-5-5") => (2.0, 10.0),
        m if m.starts_with("claude-opus-5-5") => (4.0, 20.0),
        _ => (0.0, 0.0),
    }
}

/// USD per million tokens (input, output) for OpenAI's models (standard tier, November 2026).
fn openai_price(model: &str) -> (f64, f64) {
    match model {
        "gpt-6-luna" => (0.10, 0.50),
        "gpt-6.1-terra" => (1.0, 5.0),
        "gpt-6.2-sol" => (2.0, 10.0),
        _ => (0.0, 0.0),
    }
}

/// The text of an OpenAI Responses API answer: its messages' `output_text` parts. None for a refusal.
fn openai_text(v: &Value) -> Option<String> {
    let parts: Vec<&Value> = v["output"]
        .as_array()
        .map(|items| items.iter().filter(|i| i["type"] == "message").filter_map(|i| i["content"].as_array()).flatten().collect())
        .unwrap_or_default();
    if parts.iter().any(|p| p["type"] == "refusal") {
        return None;
    }
    Some(parts.iter().filter(|p| p["type"] == "output_text").filter_map(|p| p["text"].as_str()).collect())
}

/// One answer from an OpenAI-compatible server's Chat Completions API (`<base>/chat/completions`): Ollama,
/// LM Studio, llama.cpp's server and the rest all speak it. `key` may be empty (a local server needs none).
pub fn chat_completions(http: &reqwest::blocking::Client, base: &str, key: &str, model: &str, system: &str, prompt: &str, max_tokens: u32) -> Result<String> {
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let body = json!({
        "model": model,
        "messages": [{ "role": "system", "content": system }, { "role": "user", "content": prompt }],
        "temperature": 0.2,
        "max_tokens": max_tokens,
        "stream": false,
    });
    let mut req = http.post(&url).json(&body).timeout(std::time::Duration::from_secs(60));
    if !key.is_empty() {
        req = req.bearer_auth(key);
    }
    let resp = req.send().with_context(|| format!("no answer from {base}: is the server running?"))?;
    let code = resp.status();
    let text = resp.text().unwrap_or_default();
    if !code.is_success() {
        let snippet: String = text.chars().take(200).collect();
        bail!("{base} answered HTTP {code}: {snippet}");
    }
    let v: Value = serde_json::from_str(&text).with_context(|| format!("{base} didn't answer in the OpenAI format"))?;
    let content = v["choices"][0]["message"]["content"].as_str().ok_or_else(|| anyhow!("{base}: no message in the answer"))?;
    // (a reasoning model's thinking, <think>…</think>, before its answer: not the translation)
    let content = match content.rfind("</think>") {
        Some(i) => &content[i + "</think>".len()..],
        None => content,
    };
    Ok(content.trim().to_string())
}

/// The offline translator's threads: the setting's (1–8), or for 0 as suits this computer: 2 on
/// 8 cores or fewer, 4 on more.
pub fn ort_threads(setting: u32) -> usize {
    match setting {
        1..=8 => setting as usize,
        _ => {
            let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
            if cores <= 8 { 2 } else { 4 }
        }
    }
}

/// The translators that are language models (one prompt with tags, a cost per line).
pub fn is_llm(engine: &str) -> bool {
    engine == "claude" || engine == "openai"
}

/// What a line is said in, for the language models (the other translators take each line alone).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum LineContext {
    /// A spoken line or an instruction: the lines just before it are its context, and it becomes
    /// theirs for the next one.
    #[default]
    Spoken,
    /// Text on its own (a menu's word, a screen's title): no earlier lines, and it isn't one for the
    /// next spoken line either.
    Alone,
    /// Text with what is round it on the same screen (the phone's chat: who sent each message), as
    /// `Alone` otherwise.
    Given(String),
}

pub struct Translator {
    http: reqwest::blocking::Client,
    model_dir: PathBuf,
    local: Option<LocalMt>,
    cache: HashMap<String, Segments>,
    /// Arabic words for a highlighted English phrase (see `recolor`)
    phrase_words: HashMap<String, Vec<String>>,
    /// target language the local model, phrase words and context are for
    lang: String,
    history: VecDeque<(String, String)>,
    /// what the next lines are said in (`set_context`)
    context: LineContext,
    pub cost_usd: f64,
    pub lines: u64,
    /// the offline model failed to download or load, when and why: not tried again on every line
    local_failed: Option<(Instant, String)>,
}

impl Translator {
    pub fn new(model_dir: PathBuf) -> Self {
        let http = crate::net::http_client()
            .user_agent("vi-translator/0.1")
            .timeout(Duration::from_secs(20))
            .build()
            .expect("http client");
        Self { http, model_dir, local: None, cache: HashMap::new(), phrase_words: HashMap::new(), lang: String::new(), history: VecDeque::new(), context: LineContext::Spoken, cost_usd: 0.0, lines: 0, local_failed: None }
    }

    /// What the next lines are said in, until it's set again.
    pub fn set_context(&mut self, c: LineContext) {
        self.context = c;
    }

    /// `line` may contain button-icon placeholders "⟦n⟧" (see `ocr::text_with_icons`); `icons[n-1]` is
    /// the icon image. The icons come back as segments `("", Some("icon:<png data url>"))`.
    /// Translate a game line. Names and terms that must stay as they are (GTA+, GTA$500,000,
    /// web addresses …) go through as "[1]", "[2]" — the one form the local model never changes —
    /// and are put back afterwards.
    pub fn translate(
        &mut self,
        cfg: &Config,
        line: &str,
        phrases: &[(String, String)],
        icons: &[String],
        status: &dyn Fn(String),
    ) -> Result<Segments> {
        // another target language: its own model, word lists and context
        if self.lang != cfg.target_lang {
            self.lang = cfg.target_lang.clone();
            self.local = None;
            self.local_failed = None;
            self.phrase_words.clear();
            self.history.clear();
        }
        let (protected, terms) = protect_terms(line);
        // nothing but kept terms ("GTA+"): nothing to translate
        let rest: String = protected.chars().filter(|c| c.is_alphabetic()).collect();
        if !terms.is_empty() && rest.is_empty() {
            return Ok(vec![(format!("\u{2066}{line}\u{2069}"), None)]);
        }
        // The free offline model into Arabic: the glossary's whole lines as they are, its names as
        // "[n]" (put back in Arabic below), and its Arabic tidied (crate::glossary)
        // (the glossary's whole lines, the names kept, the clean-up: for the offline translator and Apple's alike)
        let offline_ar = (cfg.engine == "local" || cfg.engine == "apple") && cfg.target_lang == "ar";
        if offline_ar && icons.is_empty() && phrases.is_empty() {
            if let Some(ar) = crate::glossary::whole_line(&cfg.glossary, line) {
                return Ok(vec![(ar, None)]);
            }
        }
        let (protected, glossed) = if offline_ar { crate::glossary::protect(&cfg.glossary, &protected, terms.len()) } else { (protected, Vec::new()) };
        let mut segs = self.translate_inner(cfg, &protected, phrases, icons, status)?;
        if !glossed.is_empty() {
            const AR_DIGITS: [char; 10] = ['٠', '١', '٢', '٣', '٤', '٥', '٦', '٧', '٨', '٩'];
            for seg in segs.iter_mut().filter(|s| !is_icon(s)) {
                for (i, ar) in glossed.iter().enumerate() {
                    let n = (terms.len() + i + 1).to_string();
                    let an: String = n.chars().map(|c| AR_DIGITS[c.to_digit(10).unwrap_or(0) as usize]).collect();
                    for form in [format!("[{n}]"), format!("]{n}["), format!("[{an}]"), format!("]{an}[")] {
                        seg.0 = seg.0.replace(&form, ar);
                    }
                }
            }
        }
        if offline_ar {
            for seg in segs.iter_mut().filter(|s| !is_icon(s)) {
                seg.0 = crate::glossary::tidy(line, &seg.0);
            }
            segs.retain(|s| is_icon(s) || !s.0.is_empty());
        }
        // (the phrasebook's hand-written lines carry no colours, and the translator may have missed one)
        segs = self.recolor(cfg, segs, phrases, status);
        if !terms.is_empty() {
            const AR_DIGITS: [char; 10] = ['٠', '١', '٢', '٣', '٤', '٥', '٦', '٧', '٨', '٩'];
            for seg in segs.iter_mut() {
                for (i, t) in terms.iter().enumerate() {
                    let n = (i + 1).to_string();
                    let an: String = n.chars().map(|c| AR_DIGITS[c.to_digit(10).unwrap_or(0) as usize]).collect();
                    // the model may answer in Arabic digits, and right-to-left text can flip the brackets
                    // (inside an invisible left-to-right isolate, so right-to-left text can't turn
                    // "GTA+" into "+GTA")
                    let kept = format!("\u{2066}{t}\u{2069}");
                    for form in [format!("[{n}]"), format!("]{n}["), format!("[{an}]"), format!("]{an}[")] {
                        seg.0 = seg.0.replace(&form, &kept);
                    }
                }
            }
        }
        let segs = polite(&cfg.target_lang, segs);
        // An answer in English instead (a model's note on a licence plate or a jumble of letters: "the
        // line can't be translated") is never drawn over the game.
        let lang = crate::langs::get(&cfg.target_lang);
        let out: String = segs.iter().map(|s| s.0.as_str()).collect();
        if english_leak(line, &out, lang.native) {
            bail!("the translation came back in English, not shown: {out}");
        }
        // The offline model makes up a whole sentence for a few stray letters (a licence plate's edge
        // read as "I.JGO" -> a heading about an anti-corruption expert group): far longer than the line.
        if cfg.engine == "local" && made_up(line, &out) {
            bail!("the translation is far longer than the line, not shown: {out}");
        }
        Ok(segs)
    }

    fn translate_inner(
        &mut self,
        cfg: &Config,
        line: &str,
        phrases: &[(String, String)],
        icons: &[String],
        status: &dyn Fn(String),
    ) -> Result<Segments> {
        // Slang machine translators mistake for a name ("C'mon" -> "سيد (كومون)"): give them plain words.
        let expanded = expand_slang(line);
        let line = expanded.as_str();
        let icon_sig: usize = icons.iter().map(|i| i.len()).sum();
        // (a line with a screen round it is its own: "Where are you?" in one chat isn't in another)
        let around = match &self.context {
            LineContext::Given(t) if is_llm(&cfg.engine) => t.as_str(),
            _ => "",
        };
        // (the player's glossary changes the offline model's answer: a new one is a new key)
        let glossary = if cfg.engine == "local" || cfg.engine == "apple" { cfg.glossary.as_str() } else { "" };
        let key = format!("{}\u{1}{}\u{1}{}\u{1}{}\u{1}{:?}\u{1}{icon_sig}\u{1}{around}\u{1}{glossary}", cfg.engine, cfg.target_lang, cfg.dialect, line, phrases);
        if let Some(s) = self.cache.get(&key) {
            return Ok(s.clone());
        }
        // The game's common instructions: exact, hand-written translations from its layout update.
        if let Some(segs) = crate::phrasebook::find(&cfg.game(), &cfg.target_lang, line, icons) {
            self.lines += 1;
            self.cache.insert(key, segs.clone());
            return Ok(segs);
        }
        let segs = if cfg.engine == "claude" {
            self.claude(cfg, line, phrases, icons)?
        } else if cfg.engine == "openai" {
            self.openai(cfg, line, phrases, icons)?
        } else {
            // Machine translators can't be trusted to keep markers, so translate the line without
            // the icons and put each icon back after the Arabic for the English word before it.
            let (plain, anchors) = icon_anchors(line, icons.len());
            let arabic = cfg.target_lang == "ar";
            // One batch: the line, plus each highlighted phrase in a few forms ("bank" alone is
            // ambiguous, "the bank" isn't) so we can find which Arabic words to colour.
            let mut batch = vec![plain.clone()];
            let mut groups = Vec::new();
            for (phrase, color) in phrases {
                let bare = phrase.trim_matches(|c: char| " .,!?;:\"'".contains(c)).to_string();
                let variants: Vec<String> = {
                    let mut v = vec![phrase.clone(), bare.clone(), format!("the {bare}")];
                    v.dedup();
                    v
                };
                groups.push((batch.len(), variants.len(), color.clone()));
                batch.extend(variants);
            }
            // The offline model translates one sentence well and a paragraph badly (a menu's help text: it
            // drifts and invents, even Bible verses): several sentences go one by one, in the same batch.
            let parts = if (cfg.engine == "local" || cfg.engine == "apple") && icons.is_empty() && phrases.is_empty() { sentences(&plain) } else { Vec::new() };
            let (out, sentence) = if parts.len() > 1 {
                let out = self.mt_batch(cfg, &parts, status)?;
                let joined = out.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
                (out, joined)
            } else {
                let out = self.mt_batch(cfg, &batch, status)?;
                let first = out.first().cloned().unwrap_or_default();
                (out, first)
            };
            let mut targets: Vec<(Vec<String>, String)> = Vec::new();
            for (start, len, color) in groups {
                let words = out[start..start + len]
                    .iter()
                    .flat_map(|t| t.split_whitespace().map(ar_norm).collect::<Vec<_>>())
                    .filter(|w| !w.is_empty())
                    .collect();
                targets.push((words, color));
            }
            let segs = color_tokens(&sentence, &targets);
            if icons.is_empty() {
                segs
            } else {
                // Anchor words that aren't known button verbs: translate them alone to find them.
                let unknown: Vec<String> =
                    anchors.iter().filter_map(|a| a.prev.clone()).filter(|w| !arabic || verb_stems(w).is_none()).collect();
                let found = if unknown.is_empty() { Vec::new() } else { self.mt_batch(cfg, &unknown, status)? };
                let lookup: HashMap<String, Vec<String>> = unknown
                    .into_iter()
                    .zip(found)
                    .map(|(en, ar)| (en, ar.split_whitespace().map(ar_norm).filter(|w| !w.is_empty()).collect()))
                    .collect();
                place_icons(segs, &anchors, icons, &lookup, arabic)
            }
        };
        self.lines += 1;
        self.cache.insert(key, segs.clone());
        Ok(segs)
    }

    /// Colour the game's highlighted words ("Go to <green>Lamar</green>") in an Arabic line that has
    /// no colours: a line from memory saved before (or without) its highlight was
    /// seen, or a hand-written phrasebook line. Each phrase is translated on its own (a few forms,
    /// cached) and the matching Arabic words take its colour. Anything that fails leaves the line
    /// as it was.
    pub fn recolor(&mut self, cfg: &Config, segs: Segments, phrases: &[(String, String)], status: &dyn Fn(String)) -> Segments {
        let has_color = segs.iter().any(|s| s.1.as_deref().is_some_and(|c| !c.starts_with("icon:")));
        if phrases.is_empty() || has_color || is_llm(&cfg.engine) {
            return segs;
        }
        let mut targets: Vec<(Vec<String>, String)> = Vec::new();
        for (phrase, color) in phrases {
            let bare = phrase.trim_matches(|c: char| " .,!?;:\"'".contains(c)).to_string();
            if bare.is_empty() {
                continue;
            }
            let words = match self.phrase_words.get(&bare.to_lowercase()) {
                Some(w) => w.clone(),
                None => {
                    let mut variants = vec![bare.clone(), format!("the {bare}")];
                    if *phrase != bare {
                        variants.push(phrase.clone());
                    }
                    let Ok(out) = self.mt_batch(cfg, &variants, status) else { return segs };
                    let w: Vec<String> =
                        out.iter().flat_map(|t| t.split_whitespace().map(ar_norm).collect::<Vec<_>>()).filter(|w| !w.is_empty()).collect();
                    self.phrase_words.insert(bare.to_lowercase(), w.clone());
                    w
                }
            };
            if !words.is_empty() {
                targets.push((words, color.clone()));
            }
        }
        if targets.is_empty() {
            return segs;
        }
        let mut out: Segments = Vec::new();
        for (text, c) in segs {
            if c.is_some() || text.trim().is_empty() {
                out.push((text, c));
                continue;
            }
            // keep the spaces around this piece (an icon may sit on either side)
            if text.starts_with(char::is_whitespace) {
                out.push((" ".into(), None));
            }
            out.extend(color_tokens(&text, &targets));
            if text.ends_with(char::is_whitespace) {
                out.push((" ".into(), None));
            }
        }
        out
    }

    pub fn stats(&self, cfg: &Config) -> String {
        if is_llm(&cfg.engine) {
            format!("{} lines, ${:.4}", self.lines, self.cost_usd)
        } else {
            format!("{} lines", self.lines)
        }
    }

    fn mt_batch(&mut self, cfg: &Config, texts: &[String], status: &dyn Fn(String)) -> Result<Vec<String>> {
        match cfg.engine.as_str() {
            // Apple's on-device translation (macOS 26): on any failure the offline translator takes the line,
            // and the reason is in the log
            "apple" => {
                let now = Instant::now();
                let tried = APPLE_BREAKER.lock().unwrap_or_else(std::sync::PoisonError::into_inner).allows(now);
                let got = if tried { crate::apple_translate::translate(crate::langs::get(&cfg.target_lang).code, texts) } else { Err(anyhow!("paused after repeated failures")) };
                match got {
                    Ok(out) => {
                        APPLE_BREAKER.lock().unwrap_or_else(std::sync::PoisonError::into_inner).succeeded();
                        Ok(out)
                    }
                    Err(e) => {
                        if tried {
                            APPLE_BREAKER.lock().unwrap_or_else(std::sync::PoisonError::into_inner).failed(now);
                        }
                        crate::vlog!("[apple] {e:#}: the offline translator takes this line");
                        let mut local = cfg.clone();
                        local.engine = "local".into();
                        self.mt_batch(&local, texts, status)
                    }
                }
            }
            "local" => {
                if self.local.is_none() {
                    // (a download that failed is tried again after a while, not on every line)
                    const RETRY: Duration = Duration::from_secs(180);
                    if let Some((at, err)) = &self.local_failed {
                        let waited = at.elapsed();
                        if waited < RETRY {
                            bail!("{err} (trying again in {} s)", (RETRY - waited).as_secs() + 1);
                        }
                    }
                    let lang = crate::langs::get(&cfg.target_lang);
                    // (Arabic's model stays where it always was; the others get their own folder)
                    let dir = if lang.code == "ar" { self.model_dir.clone() } else { self.model_dir.join(lang.code) };
                    match LocalMt::load(&dir, lang, &self.http, status, ort_threads(cfg.ort_threads)) {
                        Ok(mt) => {
                            self.local = Some(mt);
                            self.local_failed = None;
                        }
                        Err(e) => {
                            if lang.local.is_some() {
                                self.local_failed = Some((Instant::now(), format!("{e:#}")));
                            }
                            return Err(e);
                        }
                    }
                    status("Free local translator ready.".into());
                }
                let mt = self.local.as_mut().unwrap();
                texts.iter().map(|t| mt.translate(t)).collect()
            }
            "deepl" => {
                need(&cfg.deepl_key, "DeepL API key")?;
                let host = if cfg.deepl_key.ends_with(":fx") { "api-free.deepl.com" } else { "api.deepl.com" };
                let v = self.post_json(
                    self.http
                        .post(format!("https://{host}/v2/translate"))
                        .header("Authorization", format!("DeepL-Auth-Key {}", cfg.deepl_key))
                        .json(&json!({ "text": texts, "source_lang": "EN", "target_lang": crate::langs::get(&cfg.target_lang).deepl })),
                )?;
                strings(&v["translations"], |t| t["text"].as_str())
            }
            "azure" => {
                need(&cfg.azure_key, "Azure Translator key")?;
                let mut req = self
                    .http
                    .post(format!(
                        "https://api.cognitive.microsofttranslator.com/translate?api-version=3.0&from=en&to={}",
                        crate::langs::get(&cfg.target_lang).azure
                    ))
                    .header("Ocp-Apim-Subscription-Key", &cfg.azure_key);
                if !cfg.azure_region.trim().is_empty() {
                    req = req.header("Ocp-Apim-Subscription-Region", cfg.azure_region.trim());
                }
                let body: Vec<Value> = texts.iter().map(|t| json!({ "Text": t })).collect();
                let v = self.post_json(req.json(&body))?;
                strings(&v, |t| t["translations"][0]["text"].as_str())
            }
            "google" => {
                need(&cfg.google_key, "Google Cloud API key")?;
                let v = self.post_json(
                    self.http
                        .post("https://translation.googleapis.com/language/translate/v2")
                        .header("X-goog-api-key", &cfg.google_key)
                        .json(&json!({ "q": texts, "source": "en", "target": crate::langs::get(&cfg.target_lang).google, "format": "text" })),
                )?;
                strings(&v["data"]["translations"], |t| t["translatedText"].as_str())
            }
            "libre" => {
                need(&cfg.libre_url, "LibreTranslate URL")?;
                let mut body = json!({ "q": texts, "source": "en", "target": crate::langs::get(&cfg.target_lang).libre, "format": "text" });
                if !cfg.libre_key.is_empty() {
                    body["api_key"] = json!(cfg.libre_key);
                }
                let url = format!("{}/translate", cfg.libre_url.trim_end_matches('/'));
                let v = self.post_json(self.http.post(url).json(&body))?;
                match &v["translatedText"] {
                    Value::Array(_) => strings(&v["translatedText"], |t| t.as_str()),
                    Value::String(s) => Ok(vec![s.clone()]),
                    _ => bail!("Unexpected LibreTranslate reply"),
                }
            }
            other => bail!("Unknown translator \"{other}\""),
        }
    }

    fn post_json(&self, req: reqwest::blocking::RequestBuilder) -> Result<Value> {
        let resp = req.send().context("network error")?;
        let code = resp.status();
        let text = resp.text().unwrap_or_default();
        if !code.is_success() {
            let snippet: String = text.chars().take(200).collect();
            bail!("HTTP {code}: {snippet}");
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// The prompt both language models get: the system text, the user text (earlier lines for
    /// context, then the new line with its highlighted words as <c1>…</c1> and icons as <b1/>), and
    /// each highlight's colour by its number.
    fn llm_prompt(&self, cfg: &Config, line: &str, phrases: &[(String, String)], icons: &[String]) -> (String, String, HashMap<String, String>) {
        let lang = crate::langs::get(&cfg.target_lang);
        let target = if lang.code == "ar" {
            DIALECTS.iter().find(|d| d.0 == cfg.dialect).unwrap_or(&DIALECTS[0]).1.to_string()
        } else {
            format!("{} ({}), natural and readable", lang.name, lang.native)
        };
        let name = lang.name;
        let cast = if cfg.game() == "gta6" { GTA6_CAST } else { "" };
        let system = format!(
            "You translate English video-game text (subtitles and on-screen instructions) into {name} for players reading live.\n\
             Target language: {target}.\n\
             Rules:\n\
             - Output ONLY the {name} translation of the NEW line. No quotes, notes or English.\n\
             - Keep it short enough to read in 2-3 seconds; meaning and tone over literal wording.\n\
             - Translate slang, jokes and insults naturally (tone down only the most graphic slurs).\n\
             - The line came from OCR and may have small errors; silently fix obvious ones.\n\
             - Use earlier lines only as context; never re-translate them.\n\
             - Words wrapped in tags like <c1>...</c1> are highlighted in the game. Wrap the {name} words that\n\
               translate them in the same tag (same number). Never add tags that weren't in the input.\n\
             - Tags like <b1/> are controller button icons (e.g. the X or triangle button). Put each one where it\n\
               belongs in the {name} sentence, e.g. in Arabic \"Press <b1/> to jump\" -> \"اضغط <b1/> للقفز\". Keep every\n\
               icon tag exactly once.\n\
             - Use the context to get right who speaks to whom: in {name} that decides the grammatical gender\n\
               and number of \"you\", of verbs and of adjectives.{cast}"
        );
        // Tag the highlighted phrases: "Go to the <c1>bank</c1>."
        let mut tagged = line.to_string();
        for n in (1..=icons.len()).rev() {
            tagged = tagged.replace(&crate::ocr::icon_token(n), &format!("<b{n}/>"));
        }
        let mut colors = HashMap::new();
        let mut pos = 0;
        for (i, (phrase, color)) in phrases.iter().enumerate() {
            if let Some(at) = tagged[pos..].find(phrase.as_str()).map(|a| a + pos) {
                let n = i + 1;
                let wrapped = format!("<c{n}>{phrase}</c{n}>");
                tagged.replace_range(at..at + phrase.len(), &wrapped);
                pos = at + wrapped.len();
                colors.insert(n.to_string(), color.clone());
            }
        }
        let prompt = match &self.context {
            LineContext::Spoken if !self.history.is_empty() => {
                let earlier: Vec<String> = self.history.iter().map(|(en, ar)| format!("EN: {en}\nAR: {ar}")).collect();
                format!("Earlier lines:\n{}\n\nNEW line:\n{tagged}", earlier.join("\n"))
            }
            LineContext::Given(around) => {
                format!("The line is on a screen in the game. What the screen shows, for context only (do not translate it):\n{around}\n\nNEW line:\n{tagged}")
            }
            _ => format!("NEW line:\n{tagged}"),
        };
        (system, prompt, colors)
    }

    fn claude(&mut self, cfg: &Config, line: &str, phrases: &[(String, String)], icons: &[String]) -> Result<Segments> {
        need(&cfg.claude_key, "Anthropic API key")?;
        let (system, prompt, colors) = self.llm_prompt(cfg, line, phrases, icons);
        let model = cfg.claude_model.trim();
        let mut body = json!({
            "model": model,
            "max_tokens": 1024,
            "system": system,
            "messages": [{ "role": "user", "content": prompt }],
            "output_config": { "effort": "low" },
        });
        let mut req = self
            .http
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &cfg.claude_key)
            .header("anthropic-version", "2026-10-21");
        if model.starts_with("claude-haiku") {
            // Cheapest path: thinking off, so we only pay for the short Arabic line.
            body["thinking"] = json!({ "type": "disabled" });
            body["max_tokens"] = json!(300);
        } else {
            // On a policy decline, let the API retry on a fallback model in the same call.
            req = req.header("anthropic-beta", "server-side-fallback-2026-07-01");
            body["fallbacks"] = json!("default");
        }
        let v = self.post_json(req.json(&body))?;

        let (pin, pout) = claude_price(model);
        let usage = &v["usage"];
        self.cost_usd += (usage["input_tokens"].as_f64().unwrap_or(0.0) * pin
            + usage["output_tokens"].as_f64().unwrap_or(0.0) * pout)
            / 1e6;
        if v["stop_reason"] == "refusal" {
            return Ok(Vec::new());
        }
        let ar: String = v["content"]
            .as_array()
            .map(|blocks| blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect())
            .unwrap_or_default();
        Ok(self.llm_reply(&ar, &colors, icons, line))
    }

    /// ChatGPT: the OpenAI Responses API, with the same prompt as Claude. The lines aren't stored at
    /// OpenAI (`store: false`), and the small models answer without reasoning (fast, cheap).
    fn openai(&mut self, cfg: &Config, line: &str, phrases: &[(String, String)], icons: &[String]) -> Result<Segments> {
        // (a server of the player's own — a local model in Ollama or LM Studio: the Chat Completions API every
        // one of them speaks, no key needed, nothing paid)
        if !cfg.openai_url.trim().is_empty() {
            let (system, prompt, colors) = self.llm_prompt(cfg, line, phrases, icons);
            let ar = chat_completions(&self.http, cfg.openai_url.trim(), cfg.openai_key.trim(), cfg.openai_model.trim(), &system, &prompt, 400)?;
            return Ok(self.llm_reply(&ar, &colors, icons, line));
        }
        need(&cfg.openai_key, "OpenAI API key")?;
        let (system, prompt, colors) = self.llm_prompt(cfg, line, phrases, icons);
        let model = cfg.openai_model.trim();
        // ("none" isn't offered on every model: the bigger ones think a little)
        let effort = if model.contains("luna") || model.contains("terra") { "none" } else { "low" };
        let body = json!({
            "model": model,
            "instructions": system,
            "input": prompt,
            "max_output_tokens": if effort == "none" { 300 } else { 1024 },
            "reasoning": { "effort": effort },
            "store": false,
        });
        let req = self.http.post("https://api.openai.com/v1/responses").bearer_auth(cfg.openai_key.trim());
        let v = self.post_json(req.json(&body))?;

        let (pin, pout) = openai_price(model);
        let usage = &v["usage"];
        self.cost_usd += (usage["input_tokens"].as_f64().unwrap_or(0.0) * pin
            + usage["output_tokens"].as_f64().unwrap_or(0.0) * pout)
            / 1e6;
        // (a refusal: nothing shown for this line)
        let Some(ar) = openai_text(&v) else { return Ok(Vec::new()) };
        Ok(self.llm_reply(&ar, &colors, icons, line))
    }

    /// Snapshots with Claude or ChatGPT: the whole picture (a JPEG, base64) and the text blocks the
    /// app read on it (number, English), in one request. The model sees where each block is and what is
    /// around it, fixes what OCR misread, and answers each block's translation by its number. Blocks it
    /// finds aren't real text come back empty.
    pub fn picture(&mut self, cfg: &Config, jpeg_b64: &str, blocks: &[(usize, String)]) -> Result<HashMap<usize, String>> {
        let lang = crate::langs::get(&cfg.target_lang);
        let target = if lang.code == "ar" {
            DIALECTS.iter().find(|d| d.0 == cfg.dialect).unwrap_or(&DIALECTS[0]).1.to_string()
        } else {
            format!("{} ({}), natural and readable", lang.name, lang.native)
        };
        let name = lang.name;
        let system = format!(
            "You translate the text on a video-game screenshot into {name}, like a translated photo.\n\
             Target language: {target}.\n\
             You get the screenshot and a numbered list of the text blocks an OCR read on it, top to bottom.\n\
             Rules:\n\
             - Answer ONLY a JSON object: {{\"1\": \"translation\", \"2\": \"translation\", ...}}, one entry per block number.\n\
             - The OCR may have misread letters: use the picture to read each block correctly, then translate it.\n\
             - Use the whole screen as context (a menu, a map, a phone, a mission briefing) for the right words.\n\
             - Keep names of people, places, brands, vehicles and numbers as they are.\n\
             - Menu items, tabs and buttons stay short; sentences read naturally.\n\
             - A block that is not real text (OCR noise, a logo) gets an empty string."
        );
        let list: Vec<String> = blocks.iter().map(|(n, en)| format!("{n}: {en}")).collect();
        let prompt = format!("Text blocks:\n{}", list.join("\n"));
        let text = if cfg.engine == "openai" && !cfg.openai_url.trim().is_empty() {
            // (a local model: the text blocks only — most local models see no pictures)
            chat_completions(&self.http, cfg.openai_url.trim(), cfg.openai_key.trim(), cfg.openai_model.trim(), &system, &prompt, 4000)?
        } else if cfg.engine == "openai" {
            need(&cfg.openai_key, "OpenAI API key")?;
            let model = cfg.openai_model.trim();
            let effort = if model.contains("luna") || model.contains("terra") { "none" } else { "low" };
            let body = json!({
                "model": model,
                "instructions": system,
                "input": [{ "role": "user", "content": [
                    { "type": "input_image", "image_url": format!("data:image/jpeg;base64,{jpeg_b64}") },
                    { "type": "input_text", "text": prompt },
                ]}],
                "max_output_tokens": 8000,
                "reasoning": { "effort": effort },
                "store": false,
            });
            let req = self.http.post("https://api.openai.com/v1/responses").bearer_auth(cfg.openai_key.trim());
            let v = self.post_json(req.json(&body))?;
            let (pin, pout) = openai_price(model);
            self.cost_usd += (v["usage"]["input_tokens"].as_f64().unwrap_or(0.0) * pin + v["usage"]["output_tokens"].as_f64().unwrap_or(0.0) * pout) / 1e6;
            openai_text(&v).unwrap_or_default()
        } else {
            need(&cfg.claude_key, "Anthropic API key")?;
            let model = cfg.claude_model.trim();
            let mut body = json!({
                "model": model,
                "max_tokens": 8000,
                "system": system,
                "messages": [{ "role": "user", "content": [
                    { "type": "image", "source": { "type": "base64", "media_type": "image/jpeg", "data": jpeg_b64 } },
                    { "type": "text", "text": prompt },
                ]}],
                "output_config": { "effort": "low" },
            });
            if model.starts_with("claude-haiku") {
                body["thinking"] = json!({ "type": "disabled" });
            }
            let req = self.http.post("https://api.anthropic.com/v1/messages").header("x-api-key", &cfg.claude_key).header("anthropic-version", "2026-10-21");
            let v = self.post_json(req.json(&body))?;
            let (pin, pout) = claude_price(model);
            self.cost_usd += (v["usage"]["input_tokens"].as_f64().unwrap_or(0.0) * pin + v["usage"]["output_tokens"].as_f64().unwrap_or(0.0) * pout) / 1e6;
            if v["stop_reason"] == "refusal" {
                bail!("the model declined to translate this picture");
            }
            v["content"].as_array().map(|b| b.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect()).unwrap_or_default()
        };
        // the JSON object in the answer (a model may wrap it in a code fence)
        let (a, b) = (text.find('{'), text.rfind('}'));
        let Some(obj) = a.zip(b).filter(|(a, b)| a < b).map(|(a, b)| &text[a..=b]) else {
            bail!("the answer had no translations: {}", text.chars().take(160).collect::<String>());
        };
        let v: Value = serde_json::from_str(obj).context("the answer wasn't valid JSON")?;
        Ok(v.as_object().map(|m| m.iter().filter_map(|(k, v)| Some((k.trim().parse().ok()?, v.as_str()?.trim().to_string()))).collect()).unwrap_or_default())
    }

    /// Snapshots with Gemini's image model ("Nano Banana"): the picture goes to Google with an instruction
    /// to put every bit of its text in the target language, keeping everything else as it is, and comes
    /// back redrawn (the Interactions API).
    pub fn gemini_image(&self, key: &str, model: &str, lang: &str, jpeg: &[u8], w: u32, h: u32) -> Result<(Vec<u8>, String)> {
        use base64::Engine as _;
        need(key, "Gemini API key")?;
        let l = crate::langs::get(lang);
        let prompt = format!(
            "This is a screenshot of a video game. Make the same screenshot as if the game were localized into {name} ({native}).\n\
             - Replace every piece of English text with its {name} translation: the phone and its apps, menus, subtitles, \
             instructions, buttons and their labels, prompts, signs and logos on the screen. Translate naturally, as a \
             professional game localization would; write names in {name} script.\n\
             - Set the {name} text in the same place, size, colour, weight and style as the English it replaces{rtl}.\n\
             - Change NOTHING else: keep the same composition, people, faces, vehicles, roads, sky, buildings, the minimap \
             and its icons, the speedometer and every number, button icons and colours exactly as they are.\n\
             - Keep the image's full frame and aspect ratio.",
            name = l.name,
            native = l.native,
            rtl = if l.rtl { ", right-to-left and right-aligned where the English was left-aligned" } else { "" },
        );
        let ratio = w as f64 / h.max(1) as f64;
        let aspect = [("16:9", 16.0 / 9.0), ("4:3", 4.0 / 3.0), ("3:2", 1.5), ("1:1", 1.0), ("21:9", 21.0 / 9.0), ("16:10", 1.6)]
            .iter()
            .min_by(|a, b| (a.1 - ratio).abs().total_cmp(&(b.1 - ratio).abs()))
            .map(|a| a.0)
            .unwrap_or("16:9");
        let model = if model.trim().is_empty() { "gemini-nano-banana-2.1" } else { model.trim() };
        let body = json!({
            "model": model,
            "input": [
                { "type": "text", "text": prompt },
                { "type": "image", "mime_type": "image/jpeg", "data": base64::engine::general_purpose::STANDARD.encode(jpeg) },
            ],
            "response_format": { "type": "image", "aspect_ratio": aspect, "image_size": "2K" },
        });
        let req = self
            .http
            .post("https://generativelanguage.googleapis.com/v1beta/interactions")
            .header("x-goog-api-key", key.trim())
            .header("Api-Revision", "2026-05-20")
            .timeout(std::time::Duration::from_secs(120))
            .json(&body);
        let v = self.post_json(req)?;
        // the picture: an "image" block somewhere in the answer's steps, its base64 in "data"
        fn find(v: &Value) -> Option<String> {
            match v {
                Value::Object(m) => {
                    if m.get("type").and_then(Value::as_str) == Some("image") {
                        if let Some(d) = m.get("data").and_then(Value::as_str) {
                            return Some(d.to_string());
                        }
                    }
                    m.values().find_map(find)
                }
                Value::Array(a) => a.iter().find_map(find),
                _ => None,
            }
        }
        let Some(b64) = find(&v) else {
            let why = v.to_string().chars().take(240).collect::<String>();
            bail!("Gemini answered without a picture: {why}");
        };
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim()).context("Gemini's picture")?;
        let mime = if bytes.starts_with(b"\x89PNG") { "image/png" } else if bytes.starts_with(b"RIFF") { "image/webp" } else { "image/jpeg" };
        Ok((bytes, mime.to_string()))
    }

    /// Snapshots with Azure Translator's image translation: the picture goes to the resource's own
    /// endpoint (document translation, one file at once) and comes back translated, as a picture.
    pub fn azure_image(&self, endpoint: &str, key: &str, lang: &str, png: &[u8]) -> Result<(Vec<u8>, String)> {
        need(key, "Azure key for image translation")?;
        let endpoint = endpoint.trim().trim_end_matches('/');
        if !endpoint.starts_with("https://") {
            bail!("the Azure endpoint must be your resource's address, like https://<name>.cognitiveservices.azure.com");
        }
        let to = crate::langs::get(lang).azure;
        let url = format!("{endpoint}/translator/document:translate?api-version=2026-03-01&sourceLanguage=en&targetLanguage={to}");
        let boundary = "----vitranslator-snapshot-7d1f";
        let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"document\"; filename=\"snapshot.png\"\r\nContent-Type: image/png\r\n\r\n").into_bytes();
        body.extend_from_slice(png);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let resp = self
            .http
            .post(url)
            .header("Ocp-Apim-Subscription-Key", key.trim())
            .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
            .timeout(std::time::Duration::from_secs(90))
            .body(body)
            .send()
            .context("network error")?;
        let code = resp.status();
        let mime = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let bytes = resp.bytes().context("network error")?.to_vec();
        if !code.is_success() || mime.contains("json") {
            let v: Value = serde_json::from_slice(&bytes).unwrap_or_default();
            let msg = v["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| String::from_utf8_lossy(&bytes).chars().take(200).collect());
            bail!("Azure: HTTP {code}: {msg}");
        }
        let mime = if mime.starts_with("image/") { mime } else { "image/png".to_string() };
        Ok((bytes, mime))
    }

    /// A language model's answer -> segments: "<c1>…</c1>" coloured as the game highlighted them,
    /// "<b1/>" the game's button icon; the line kept as context for the next ones.
    fn llm_reply(&mut self, ar: &str, colors: &HashMap<String, String>, icons: &[String], line: &str) -> Segments {
        let ar = ar.trim();

        // Split "<c1>...</c1>" into coloured segments; drop any stray tags.
        let mut segs: Segments = Vec::new();
        let mut rest = ar;
        while let Some(open) = rest.find("<c") {
            let after = &rest[open + 2..];
            let Some(gt) = after.find('>') else { break };
            let num = &after[..gt];
            let close_tag = format!("</c{num}>");
            let inner_start = open + 2 + gt + 1;
            let Some(close) = rest[inner_start..].find(&close_tag) else { break };
            segs.push((rest[..open].to_string(), None));
            segs.push((rest[inner_start..inner_start + close].to_string(), colors.get(num).cloned()));
            rest = &rest[inner_start + close + close_tag.len()..];
        }
        segs.push((rest.to_string(), None));
        let segs: Segments = segs
            .into_iter()
            .map(|(t, c)| (strip_tags(&t), c))
            .filter(|(t, _)| !t.is_empty())
            .collect();

        // Button icons: "<b1/>" -> icon segment (an icon the model dropped is added at the end).
        let segs = split_icon_tags(segs, icons);
        let plain: String = segs.iter().filter(|s| !is_icon(s)).map(|s| s.0.as_str()).collect();
        // (only spoken lines are the next spoken line's context: a menu word or a phone message isn't)
        if self.context == LineContext::Spoken {
            self.history.push_back((line.to_string(), plain));
            while self.history.len() > 3 {
                self.history.pop_front();
            }
        }
        segs
    }
}

fn need(v: &str, what: &str) -> Result<()> {
    if v.trim().is_empty() { bail!("Enter your {what} in Settings → Translator.") } else { Ok(()) }
}

fn strings(v: &Value, f: impl Fn(&Value) -> Option<&str>) -> Result<Vec<String>> {
    v.as_array()
        .ok_or_else(|| anyhow!("Unexpected reply: {v}"))?
        .iter()
        .map(|t| f(t).map(str::to_string).ok_or_else(|| anyhow!("Unexpected reply item: {t}")))
        .collect()
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("<") {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let is_tag = tail.starts_with("<c") || tail.starts_with("</c"); // <bN/> icons are kept
        match (is_tag, tail.find('>')) {
            (true, Some(j)) => rest = &tail[j + 1..],
            _ => {
                out.push('<');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ------------------------------------------------------------------------------------- colour matching

/// Normalise an Arabic word for matching: drop diacritics/tatweel/punctuation, unify alef/ya/ta marbuta,
/// strip one leading conjunction/preposition letter and the article "ال".
pub fn ar_norm(word: &str) -> String {
    let mut w: String = word
        .chars()
        .filter(|c| !('\u{064B}'..='\u{0652}').contains(c) && *c != '\u{0640}')
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'أ' | 'إ' | 'آ' => 'ا',
            'ى' => 'ي',
            'ة' => 'ه',
            c => c,
        })
        .collect();
    if w.chars().count() > 3 && w.starts_with(['و', 'ف', 'ب', 'ل', 'ك']) {
        w = w.chars().skip(1).collect();
    }
    if w.chars().count() > 3 && w.starts_with("ال") {
        w = w.chars().skip(2).collect();
    }
    w
}

fn close_enough(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (la, lb) = (a.chars().count(), b.chars().count());
    if la.min(lb) < 3 {
        return false;
    }
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=bv.len()).collect();
    for i in 1..=av.len() {
        let mut cur = vec![i; bv.len() + 1];
        for j in 1..=bv.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(av[i - 1] != bv[j - 1]));
        }
        prev = cur;
    }
    1.0 - prev[bv.len()] as f64 / la.max(lb) as f64 >= 0.8
}

/// A paragraph's sentences: cut after . ! ? (or …) followed by a space and a capital letter
/// ("… at home. Tip: Hold …"); "e.g. 3.5 mi" stays whole.
fn sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    for i in 0..chars.len() {
        let end = matches!(chars[i], '.' | '!' | '?' | '…');
        let next = chars.get(i + 1).copied();
        let after = chars.get(i + 2).copied();
        if end && next == Some(' ') && after.is_some_and(|c| c.is_uppercase()) {
            let s: String = chars[start..=i].iter().collect();
            if !s.trim().is_empty() {
                out.push(s.trim().to_string());
            }
            start = i + 2;
        }
    }
    let rest: String = chars[start.min(chars.len())..].iter().collect();
    if !rest.trim().is_empty() {
        out.push(rest.trim().to_string());
    }
    out
}

/// Colour the words of `sentence` that match a highlighted phrase's translation.
fn color_tokens(sentence: &str, targets: &[(Vec<String>, String)]) -> Segments {
    let tokens: Vec<&str> = sentence.split_whitespace().collect();
    let mut segs: Segments = Vec::new();
    for tok in &tokens {
        let n = ar_norm(tok);
        let color = if n.is_empty() {
            None
        } else {
            targets.iter().find(|(words, _)| words.iter().any(|t| close_enough(&n, t))).map(|(_, c)| c.clone())
        };
        match segs.last_mut() {
            None => segs.push((tok.to_string(), color)),
            // Same colour as the previous run: extend it ("منزل فرانكلين" stays one coloured run).
            Some(last) if last.1 == color => {
                last.0.push(' ');
                last.0.push_str(tok);
            }
            // Uncoloured word after a coloured run: the space goes with the plain word.
            Some(_) if color.is_none() => segs.push((format!(" {tok}"), None)),
            // Coloured word after plain text: keep the space uncoloured.
            Some(_) => {
                segs.push((" ".into(), None));
                segs.push((tok.to_string(), color));
            }
        }
    }
    segs
}


// ------------------------------------------------------------------------------------- button icons

/// Where an icon sat in the English line.
pub struct IconAnchor {
    /// The English word just before the icon (lower-case, no punctuation); None = start of the line.
    pub prev: Option<String>,
    /// The icon was the last thing on the line.
    pub at_end: bool,
}

/// Remove "⟦n⟧" placeholders: returns the plain line and, for each icon in order, its anchor.
fn icon_anchors(line: &str, n_icons: usize) -> (String, Vec<IconAnchor>) {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut plain = Vec::new();
    let mut anchors: Vec<(usize, IconAnchor)> = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        let num = t.strip_prefix('⟦').and_then(|r| r.strip_suffix('⟧')).and_then(|n| n.parse::<usize>().ok());
        match num {
            Some(n) if n >= 1 && n <= n_icons => {
                let prev = plain.last().map(|w: &&str| {
                    w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
                });
                let at_end = tokens[i + 1..].iter().all(|t| t.starts_with('⟦'));
                anchors.push((n, IconAnchor { prev: prev.filter(|p| !p.is_empty()), at_end }));
            }
            _ => plain.push(*t),
        }
    }
    anchors.sort_by_key(|(n, _)| *n);
    (plain.join(" "), anchors.into_iter().map(|(_, a)| a).collect())
}

/// Arabic stems for the verbs that come before button icons ("Press ✕", "Hold △", ...).
fn verb_stems(en: &str) -> Option<&'static [&'static str]> {
    Some(match en {
        "press" | "push" | "mash" | "click" => &["اضغط", "انقر", "ادفع"],
        "hold" => &["اضغط", "استمر", "امسك", "ابق"],
        "tap" => &["انقر", "اضغط"],
        "use" => &["استخدم", "استعمل"],
        "release" => &["حرر", "افلت", "اترك", "ارفع"],
        "pull" => &["اسحب"],
        "select" | "choose" => &["اختر", "حدد"],
        "toggle" | "switch" => &["بدل", "غير"],
        _ => return None,
    })
}

pub fn is_icon(seg: &(String, Option<String>)) -> bool {
    seg.1.as_deref().is_some_and(|c| c.starts_with("icon:"))
}

fn icon_seg(png: &str) -> (String, Option<String>) {
    (String::new(), Some(format!("icon:{png}")))
}

/// Put icons back into machine-translated Arabic: after the Arabic for the English word before the
/// icon ("Press ✕" -> after "اضغط"); at the start or end when the icon started or ended the line.
fn place_icons(segs: Segments, anchors: &[IconAnchor], icons: &[String], lookup: &HashMap<String, Vec<String>>, arabic: bool) -> Segments {
    // the Arabic button verbs ("اضغط") only help in Arabic
    let verb_stems = |w: &str| if arabic { verb_stems(w) } else { None };
    // word-level tokens, keeping colours
    let mut toks: Vec<(String, Option<String>)> = Vec::new();
    for (t, c) in segs {
        for w in t.split_whitespace() {
            toks.push((w.to_string(), c.clone()));
        }
    }
    let mut floor = 0; // keep icons in their English order
    for (a, png) in anchors.iter().zip(icons) {
        let idx = match (&a.prev, a.at_end) {
            (None, _) => floor,
            (Some(prev), at_end) => {
                let found = (floor..toks.len()).find(|&i| {
                    if is_icon(&toks[i]) {
                        return false;
                    }
                    let n = ar_norm(&toks[i].0);
                    match verb_stems(prev) {
                        Some(stems) => stems.iter().any(|s| n.starts_with(&ar_norm(s)) || ar_norm(&toks[i].0).starts_with(s)),
                        None => lookup.get(prev).is_some_and(|ws| ws.iter().any(|w| close_enough(&n, w))),
                    }
                });
                match found {
                    Some(i) => i + 1,
                    None if at_end => toks.len(),
                    // a button verb we couldn't find: Arabic puts the verb first
                    None if verb_stems(prev).is_some() => (floor + 1).min(toks.len()),
                    None => toks.len(),
                }
            }
        };
        toks.insert(idx, icon_seg(png));
        floor = idx + 1;
    }
    // back to segments: same-colour words share a run, icons stand alone, spaces stay uncoloured
    let mut out: Segments = Vec::new();
    for (i, (w, c)) in toks.into_iter().enumerate() {
        if i > 0 {
            out.push((" ".into(), None));
        }
        let icon = c.as_deref().is_some_and(|c| c.starts_with("icon:"));
        match out.iter_mut().rev().nth(1) {
            Some(prev) if !icon && prev.1 == c && !is_icon(prev) && i > 0 => {
                out.pop(); // drop the space we just pushed
                let last = out.last_mut().unwrap();
                last.0.push(' ');
                last.0.push_str(&w);
            }
            _ => out.push((w, c)),
        }
    }
    out
}

/// Claude's reply keeps icons as "<bN/>" tags: turn them into icon segments.
fn split_icon_tags(segs: Segments, icons: &[String]) -> Segments {
    let mut out: Segments = Vec::new();
    let mut used = vec![false; icons.len()];
    for (text, color) in segs {
        let mut rest = text.as_str();
        while let Some(start) = rest.find("<b") {
            let after = &rest[start + 2..];
            let Some(end) = after.find("/>") else { break };
            let Ok(n) = after[..end].trim().parse::<usize>() else {
                out.push((rest[..start + 2].to_string(), color.clone()));
                rest = after;
                continue;
            };
            if !rest[..start].is_empty() {
                out.push((rest[..start].to_string(), color.clone()));
            }
            if let Some(png) = icons.get(n.wrapping_sub(1)) {
                out.push(icon_seg(png));
                used[n - 1] = true;
            }
            rest = &after[end + 2..];
        }
        if !rest.is_empty() {
            out.push((rest.to_string(), color));
        }
    }
    for (i, png) in icons.iter().enumerate() {
        if !used[i] {
            out.push((" ".into(), None));
            out.push(icon_seg(png));
        }
    }
    out
}

/// Spoken forms translators don't know, in their plain form ("C'mon" -> "Come on").
/// Words never shown, and the milder ones used instead, per target language: the stem, so every
/// form follows ("العاهرة", "كالعاهرة", "عاهرات" → "الساقطة", "كالساقطة", "ساقطات").
const POLITE: &[(&str, &str, &str)] = &[("ar", "عاهر", "ساقط")];

/// The translation with its harsh words replaced by milder ones (also for lines from the memory,
/// saved before a word was added).
pub fn polite(lang: &str, mut segs: Segments) -> Segments {
    for (l, from, to) in POLITE {
        if *l == lang {
            for s in segs.iter_mut().filter(|s| !is_icon(s)) {
                if s.0.contains(from) {
                    s.0 = s.0.replace(from, to);
                }
            }
        }
    }
    segs
}

pub fn expand_slang(line: &str) -> String {
    let mut out = line.to_string();
    for (from, to) in [("C'mon", "Come on"), ("c'mon", "come on"), ("C\u{2019}mon", "Come on"), ("c\u{2019}mon", "come on")] {
        out = out.replace(from, to);
    }
    out
}

/// Terms kept exactly as the game writes them: GTA+ / GTA$ amounts / GTA Online / GTAV, web
/// addresses, and ™ names. Returns the line with "[1]", "[2]" … in their place, and the terms.
pub fn protect_terms(line: &str) -> (String, Vec<String>) {
    let mut terms: Vec<String> = Vec::new();
    let mut out = Vec::new();
    let words: Vec<&str> = line.split(' ').collect();
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let core = w.trim_end_matches(|c: char| ",.;:!?)".contains(c));
        let tail = &w[core.len()..];
        let up = core.to_uppercase();
        let is_term = up.starts_with("GTA+")
            || up.starts_with("GTA$")
            || up == "GTAV"
            || core.contains('™')
            || (core.contains('.') && core.contains('/'))
            || core.ends_with(".com");
        // "GTA Online" / "GTA V" are two words
        let two = up == "GTA" && words.get(i + 1).is_some_and(|n| matches!(n.trim_end_matches(|c: char| ",.;:!?".contains(c)), "Online" | "V"));
        if two {
            let n = words[i + 1];
            let ncore = n.trim_end_matches(|c: char| ",.;:!?".contains(c));
            terms.push(format!("{core} {ncore}"));
            out.push(format!("[{}]{}", terms.len(), &n[ncore.len()..]));
            i += 2;
            continue;
        }
        if is_term {
            // OCR reads the zeros of "GTA$500,000" as letter O
            let core = if up.starts_with("GTA$") { format!("GTA${}", core[4..].replace(['O', 'o'], "0")) } else { core.to_string() };
            terms.push(core);
            out.push(format!("[{}]{tail}", terms.len()));
        } else {
            out.push(w.to_string());
        }
        i += 1;
    }
    (out.join(" "), terms)
}

#[cfg(test)]
mod term_tests {
    #[test]
    fn a_paragraph_goes_sentence_by_sentence() {
        let p = "Pay attention to your surroundings. Investigating strange noises may lead to new opportunities. Tip: Hold e.g. 3.5 mi away.";
        assert_eq!(
            super::sentences(p),
            vec!["Pay attention to your surroundings.", "Investigating strange noises may lead to new opportunities.", "Tip: Hold e.g. 3.5 mi away."]
        );
        assert_eq!(super::sentences("Take cover."), vec!["Take cover."]);
    }

    #[test]
    #[cfg(feature = "offline")]
    fn every_offline_model_has_a_fixed_version() {
        for l in crate::langs::LANGS.iter() {
            if let Some(repo) = l.local {
                let row = super::PINNED.iter().find(|p| p.0 == repo).unwrap_or_else(|| panic!("{repo} has no fixed version"));
                assert!(row.1.len() == 40 && row.1.bytes().all(|b| b.is_ascii_hexdigit()), "{repo}");
                assert!(row.2.iter().all(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())), "{repo}");
            }
        }
    }

    use super::protect_terms as p;
    use super::polite;
    #[test]
    fn harsh_words_are_softened() {
        let segs = vec![("لكنه يقود كالعاهرة، لذا...".to_string(), None), ("العاهرات".to_string(), Some("#ff0000".to_string()))];
        let out = polite("ar", segs.clone());
        assert_eq!(out[0].0, "لكنه يقود كالساقطة، لذا...");
        assert_eq!(out[1], ("الساقطات".to_string(), Some("#ff0000".to_string())));
        // other languages: as they are
        assert_eq!(polite("fr", segs.clone()), segs);
    }

    #[test]
    fn game_terms_are_kept() {
        let (t, terms) = p("In addition to the monthly GTA$500,000 deposited, GTA+ Members who've signed up get GTA Online cars. Visit rsg.ms/gtaplus for more.");
        assert_eq!(t, "In addition to the monthly [1] deposited, [2] Members who've signed up get [3] cars. Visit [4] for more.");
        assert_eq!(terms, vec!["GTA$500,000", "GTA+", "GTA Online", "rsg.ms/gtaplus"]);
        assert_eq!(p("Go to the guard.").1.len(), 0);
    }
}

/// Does this Arabic keep every term of the English that must stay as is (GTA+, GTA$ …)? An old
/// translation made before they were protected may have mangled them ("م م +").
pub fn keeps_terms(en: &str, ar: &str) -> bool {
    let ar = ar.replace(['\u{2066}', '\u{2069}'], "");
    protect_terms(en).1.iter().all(|t| ar.contains(t.as_str()))
}

#[cfg(test)]
mod local_server_tests {
    use super::*;
    use std::io::{Read, Write};

    /// A one-request server answering `reply` with `code`; returns its address and the request it got.
    fn serve(code: u16, reply: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}/v1", l.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = vec![0u8; 65536];
            let mut got = Vec::new();
            loop {
                let n = s.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
                let t = String::from_utf8_lossy(&got).to_string();
                if let Some(i) = t.find("\r\n\r\n") {
                    let len = t.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                    if got.len() >= i + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            tx.send(String::from_utf8_lossy(&got).to_string()).unwrap();
            let _ = write!(s, "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len());
        });
        (addr, rx)
    }

    #[test]
    fn a_local_server_is_asked_in_the_chat_format_without_a_key() {
        let (addr, rx) = serve(200, r#"{"choices":[{"message":{"role":"assistant","content":"<think>hmm</think> مرحبا"}}]}"#);
        let http = reqwest::blocking::Client::new();
        let got = chat_completions(&http, &addr, "", "gemma4:12b-it-qat", "sys", "Hello", 50).unwrap();
        assert_eq!(got, "مرحبا");
        let req = rx.recv().unwrap();
        assert!(req.starts_with("POST /v1/chat/completions"), "{req}");
        assert!(!req.to_lowercase().contains("authorization:"), "no key, no header: {req}");
        assert!(req.contains("\"stream\":false") && req.contains("gemma4:12b-it-qat"));
    }

    #[test]
    fn a_server_s_error_and_a_missing_server_are_said_plainly() {
        let (addr, _rx) = serve(404, r#"{"error":"model not found"}"#);
        let http = reqwest::blocking::Client::new();
        let e = chat_completions(&http, &addr, "k", "nope", "s", "p", 10).unwrap_err().to_string();
        assert!(e.contains("404") && e.contains("model not found"), "{e}");
        let e = chat_completions(&http, "http://127.0.0.1:9/v1", "", "m", "s", "p", 10).unwrap_err().to_string();
        assert!(e.contains("is the server running"), "{e}");
    }
}

#[cfg(test)]
mod openai_tests {
    use super::*;

    #[test]
    fn a_line_s_context_is_the_one_it_is_said_in() {
        let dir = std::env::temp_dir().join("vit-context-test");
        let mut tr = Translator::new(dir);
        let mut cfg = Config::default();
        cfg.target_lang = "ar".into();
        tr.history.push_back(("You coming?".into(), "جاي؟".into()));
        // a spoken line: the lines before it
        let (system, user, _) = tr.llm_prompt(&cfg, "Yeah.", &[], &[]);
        assert!(user.starts_with("Earlier lines:\nEN: You coming?\nAR: جاي؟\n\nNEW line:\nYeah."), "{user}");
        assert!(system.contains("who speaks to whom"));
        // a menu's word: alone, and not the next spoken line's context
        tr.set_context(LineContext::Alone);
        let (_, user, _) = tr.llm_prompt(&cfg, "Settings", &[], &[]);
        assert_eq!(user, "NEW line:\nSettings");
        tr.llm_reply("الإعدادات", &HashMap::new(), &[], "Settings");
        assert_eq!(tr.history.len(), 1);
        // the phone's chat: the screen round it, not the spoken lines
        tr.set_context(LineContext::Given("Title: Lucia\nSent to the player's character: Where are you?".into()));
        let (_, user, _) = tr.llm_prompt(&cfg, "Where are you?", &[], &[]);
        assert!(user.contains("for context only (do not translate it):\nTitle: Lucia\n") && !user.contains("You coming?"), "{user}");
        tr.llm_reply("وينك؟", &HashMap::new(), &[], "Where are you?");
        assert_eq!(tr.history.len(), 1);
        // spoken again: it is a context again
        tr.set_context(LineContext::Spoken);
        tr.llm_reply("أيوه.", &HashMap::new(), &[], "Yeah.");
        assert_eq!(tr.history.back().map(|h| h.0.as_str()), Some("Yeah."));
    }

    #[test]
    fn gta_vi_s_leads_are_named_for_the_grammar() {
        let tr = Translator::new(std::env::temp_dir().join("vit-cast-test"));
        let mut cfg = Config::default();
        cfg.target_lang = "ar".into();
        cfg.profile = "gta6".into();
        let (system, _, _) = tr.llm_prompt(&cfg, "Hi.", &[], &[]);
        assert!(system.contains("Jason (Jason Duval), a man") && system.contains("Lucia (Lucia Caminos), a woman"), "{system}");
    }

    #[test]
    fn reads_the_answer_text() {
        // a reasoning item first, then the message (as the Responses API returns them)
        let v = json!({"output": [
            {"type": "reasoning", "summary": []},
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "اضغط <b1/> للقفز", "annotations": []}]}
        ], "usage": {"input_tokens": 120, "output_tokens": 9}});
        assert_eq!(openai_text(&v).as_deref(), Some("اضغط <b1/> للقفز"));
        // a refusal shows nothing
        let v = json!({"output": [{"type": "message", "role": "assistant", "content": [{"type": "refusal", "refusal": "I can't help with that."}]}]});
        assert_eq!(openai_text(&v), None);
        // no output at all: empty, not an error
        assert_eq!(openai_text(&json!({})).as_deref(), Some(""));
    }

    #[test]
    fn prices_and_kinds() {
        assert_eq!(openai_price("gpt-6-luna"), (0.10, 0.50));
        assert_eq!(openai_price("something-else"), (0.0, 0.0));
        assert!(is_llm("openai") && is_llm("claude") && !is_llm("deepl"));
    }

    #[test]
    fn an_answer_in_english_is_not_a_translation() {
        assert!(super::english_leak("J26 4B7", "I cannot translate this line.", "العربية"));
        assert!(super::english_leak("ASS MAN", "This is a licence plate.", "العربية"));
        // the line's own names and terms are kept, and one stray word is no answer
        assert!(!super::english_leak("Steal the Bravado Gauntlet", "اسرق Bravado Gauntlet", "العربية"));
        assert!(!super::english_leak("J26 4B7", "طراز J26 4B7", "العربية"));
        assert!(!super::english_leak("Go to TSS", "اذهب إلى TSS office", "العربية"));
        // a language written in Latin letters: English words are its own
        assert!(!super::english_leak("Go home", "Ga naar huis", "Nederlands"));
    }

    #[test]
    fn a_whole_sentence_for_a_few_letters_is_made_up() {
        assert!(super::made_up("I.JGO", "أولاً - فريق الخبراء المشترك بين الوكالات المعني بمكافحة الفساد"));
        assert!(!super::made_up("Go", "اذهب"));
        assert!(!super::made_up("J26 4B7", "طراز J26 4B7"));
        assert!(!super::made_up("Hey! Eyes open!", "انتبهوا! افتحوا أعينكم!"));
        assert!(!super::made_up("You got something you gotta say?", "عندك شيء تريد قوله؟"));
    }
}
