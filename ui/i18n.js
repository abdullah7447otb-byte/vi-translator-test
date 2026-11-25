// The app's own interface language (separate from the language games are translated into).
// Each phrase is looked up by its English text; a phrase missing from a dictionary stays English.
// To add an interface language: add it to UI_LANGS, write ui/i18n/<code>.js and load it in index.html.
window.I18N = (() => {
  const UI_LANGS = [
    { code: "en", name: "English", rtl: false },
    { code: "ar", name: "العربية", rtl: true },
    { code: "hi", name: "हिन्दी", rtl: false },
    { code: "bn", name: "বাংলা", rtl: false },
    { code: "ur", name: "اردو", rtl: true },
    { code: "id", name: "Bahasa Indonesia", rtl: false },
    { code: "tr", name: "Türkçe", rtl: false },
    { code: "vi", name: "Tiếng Việt", rtl: false },
    { code: "fa", name: "فارسی", rtl: true },
    { code: "th", name: "ไทย", rtl: false },
    { code: "ta", name: "தமிழ்", rtl: false },
    { code: "te", name: "తెలుగు", rtl: false },
    { code: "mr", name: "मराठी", rtl: false },
    { code: "uk", name: "Українська", rtl: false },
    { code: "nl", name: "Nederlands", rtl: false },
    { code: "fil", name: "Filipino", rtl: false },
    { code: "ms", name: "Bahasa Melayu", rtl: false },
    { code: "sw", name: "Kiswahili", rtl: false },
    { code: "ro", name: "Română", rtl: false },
    { code: "el", name: "Ελληνικά", rtl: false },
    { code: "cs", name: "Čeština", rtl: false },
    { code: "fr", name: "Français", rtl: false },
    { code: "it", name: "Italiano", rtl: false },
    { code: "de", name: "Deutsch", rtl: false },
    { code: "ru", name: "Русский", rtl: false },
    { code: "ja", name: "日本語", rtl: false },
    { code: "pl", name: "Polski", rtl: false },
    { code: "ko", name: "한국어", rtl: false },
    { code: "zh-Hans", name: "简体中文", rtl: false },
    { code: "zh-Hant", name: "繁體中文", rtl: false },
    { code: "es-MX", name: "Español (México)", rtl: false },
    { code: "es-ES", name: "Español (España)", rtl: false },
    { code: "pt-BR", name: "Português (Brasil)", rtl: false },
  ];


  const DICTS = {};
  /** Register a dictionary (ui/i18n/<code>.js files call this). */
  const add = (code, dict) => { DICTS[code] = dict; };
  let lang = "en";

  /** Translate an English phrase; {name} placeholders are filled from `vars`. */
  function t(en, vars) {
    let s = (DICTS[lang] && DICTS[lang][en]) || en;
    if (vars) for (const [k, v] of Object.entries(vars)) s = s.split(`{${k}}`).join(String(v));
    return s;
  }

  /** Switch the interface language: direction of the whole window, and the static text in the page. */
  function setLang(code) {
    lang = UI_LANGS.some((l) => l.code === code) ? code : "en";
    const rtl = UI_LANGS.find((l) => l.code === lang).rtl;
    document.documentElement.lang = lang;
    document.documentElement.dir = rtl ? "rtl" : "ltr";
    apply(document);
  }

  /**
   * Static text in the page: elements marked data-i18n (text) or data-i18n-html (with markup), and
   * data-i18n-title (tooltip). The English is kept on first use, so switching back and forth works.
   */
  function apply(root) {
    root.querySelectorAll("[data-i18n]").forEach((el) => {
      el.dataset.en ??= el.textContent.trim();
      el.textContent = t(el.dataset.en);
    });
    root.querySelectorAll("[data-i18n-html]").forEach((el) => {
      el.dataset.en ??= el.innerHTML.trim();
      el.innerHTML = t(el.dataset.en);
    });
    root.querySelectorAll("[data-i18n-title]").forEach((el) => {
      el.dataset.enTitle ??= el.title;
      el.title = t(el.dataset.enTitle);
    });
  }

  /**
   * Messages the app itself writes (a capture status, a line in the activity log), by their English
   * pattern: shown in this language when one matches, its parts (names, errors) as they are.
   * (The more particular patterns first.)
   */
  const MSGS = [
      "Connecting to OBS for \"{name}\" ...",
      "Opening capture device \"{name}\" ...",
      "Capturing \"{name}\"",
      "Capturing device \"{name}\" ({size})",
      "Capturing OBS source \"{name}\" ({size}, {ms} ms per frame)",
      "Capture device \"{name}\" stopped sending frames ({error})",
      "Capture device \"{name}\" changed its picture: opening it again",
      "Capture device \"{name}\" stopped: {error}",
      "Capture device \"{name}\": {error}",
      "OBS source \"{name}\": {error}",
      "Downloading the free translation model: {file} ...",
      "Loading the translation model ...",
      "Free local translator ready.",
      "OCR unavailable: {error}",
      "No new picture for {s} s: the source stopped sending",
      "The text hasn't been read for {s} s",
      "The translator has been on one line for {s} s",
      "Screen Recording is off for VI Translator: turn it on in System Settings › Privacy & Security › Screen Recording, then open VI Translator again.",
      "Open VI Translator from its .dmg (VI Translator.app in Applications): run as a bare program, macOS doesn't let it reach the PS5 or the phone page on your home network.",
      "Playing on {name} ({pads} controller(s)) · {fps} fps · lost on the network: {net} · too late to decode: {late} (last 10 s)"
  ];
  const MSG_RE = MSGS.map((m) => {
    const names = [];
    const src = m.replace(/[.*+?^${}()|[\]\\]/g, "\\$&").replace(/\\\{(\w+)\\\}/g, (_, n) => { names.push(n); return "(.+?)"; });
    return [m, new RegExp(`^${src}$`, "s"), names];
  });
  function tMsg(s) {
    if (lang === "en" || typeof s !== "string") return s;
    for (const [m, re, names] of MSG_RE) {
      const hit = re.exec(s);
      if (hit) return t(m, Object.fromEntries(names.map((n, i) => [n, hit[i + 1]])));
    }
    return s;
  }

  return { UI_LANGS, add, t, tMsg, setLang, apply, get lang() { return lang; } };
})();
window.t = window.I18N.t;
