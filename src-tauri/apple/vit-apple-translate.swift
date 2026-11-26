// The app's helper for Apple's on-device translation (macOS 26 and later): newline-delimited JSON on
// stdin/stdout, one request a line, answered in order.
//
//   {"id": 1, "src": "en", "dst": "ar", "texts": ["…", …]}  ->  {"id": 1, "out": ["…", …]}
//   {"id": 2, "src": "en", "dst": "ar", "warm": true}        ->  {"id": 2, "ok": true}   (session made, model loaded)
//   {"id": 3, "status": true, "src": "en", "dst": "ar"}     ->  {"id": 3, "status": "installed" | "supported" | "unsupported"}
//   anything wrong                                         ->  {"id": n, "error": "…"}
//
// `vit-apple-translate --status en ar` prints the status alone and exits.
//
// One session per language pair, reused; requests one at a time (no concurrent use of a session). A
// session that fails (after sleep, a model update, a cancelled session) is dropped and made again, and the
// request tried once more with the new one: a bad session never stays. Several lines go in one batch.
// The handling follows CouchSubs' Translator.swift (AGPL-3.0, (C) 2026 Sinan Yorulmaz, commit bc54ff7:
// core/Sources/CouchSubsCore/Pipeline/Translator.swift).

import Foundation
#if canImport(Translation)
import Translation
#endif

func reply(_ object: [String: Any]) {
    if let data = try? JSONSerialization.data(withJSONObject: object), let line = String(data: data, encoding: .utf8) {
        FileHandle.standardOutput.write((line + "\n").data(using: .utf8)!)
    }
}

#if canImport(Translation)
@available(macOS 26.0, *)
final class Engine {
    private var sessions: [String: TranslationSession] = [:]

    func status(_ src: String, _ dst: String) async -> String {
        switch await LanguageAvailability().status(from: Locale.Language(identifier: src), to: Locale.Language(identifier: dst)) {
        case .installed: return "installed"
        case .supported: return "supported"
        default: return "unsupported"
        }
    }

    private func session(_ src: String, _ dst: String) async throws -> TranslationSession {
        let key = src + ">" + dst
        if let s = sessions[key] { return s }
        let st = await status(src, dst)
        guard st == "installed" else {
            throw NSError(domain: "vit", code: 1, userInfo: [NSLocalizedDescriptionKey: st == "supported" ? "the language pack isn't downloaded" : "this language pair isn't supported"])
        }
        let s = TranslationSession(installedSource: Locale.Language(identifier: src), target: Locale.Language(identifier: dst))
        sessions[key] = s
        return s
    }

    private func drop(_ src: String, _ dst: String) {
        sessions[src + ">" + dst] = nil
    }

    /// The session made and its model loaded, so the first real line is quick.
    func warm(_ src: String, _ dst: String) async throws {
        do {
            try await session(src, dst).prepareTranslation()
        } catch {
            drop(src, dst)
            try await session(src, dst).prepareTranslation()
        }
    }

    private func run(_ s: TranslationSession, _ texts: [String]) async throws -> [String] {
        var out = Array(repeating: "", count: texts.count)
        let wanted = texts.enumerated().filter { !$0.element.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
        if wanted.isEmpty { return out }
        if wanted.count == 1, let only = wanted.first {
            out[only.offset] = try await s.translate(only.element).targetText
            return out
        }
        // several lines: one batch, matched back by their identifiers
        let requests = wanted.map { TranslationSession.Request(sourceText: $0.element, clientIdentifier: String($0.offset)) }
        let responses = try await s.translations(from: requests)
        var filled = 0
        for r in responses {
            if let id = r.clientIdentifier, let i = Int(id), i >= 0, i < out.count {
                out[i] = r.targetText
                filled += 1
            }
        }
        if filled < wanted.count {
            // (an answer missing from the batch: those lines one by one)
            for w in wanted where out[w.offset].isEmpty {
                out[w.offset] = try await s.translate(w.element).targetText
            }
        }
        return out
    }

    func translate(_ src: String, _ dst: String, _ texts: [String]) async throws -> [String] {
        do {
            return try await run(try await session(src, dst), texts)
        } catch {
            // the session may have gone bad: a new one, and once more
            drop(src, dst)
            return try await run(try await session(src, dst), texts)
        }
    }
}
#endif

@main
struct Main {
    static func main() async {
        let args = CommandLine.arguments
        #if canImport(Translation)
        guard #available(macOS 26.0, *) else {
            if args.count >= 2, args[1] == "--status" { print("no-os") } else {
                while let line = readLine() {
                    let id = (try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any])?["id"] ?? 0
                    reply(["id": id, "error": "Apple translation needs macOS 26 or later"])
                }
            }
            return
        }
        let engine = Engine()
        if args.count >= 4, args[1] == "--status" {
            print(await engine.status(args[2], args[3]))
            return
        }
        while let line = readLine() {
            guard let request = (try? JSONSerialization.jsonObject(with: Data(line.utf8))) as? [String: Any] else {
                reply(["id": 0, "error": "not JSON"])
                continue
            }
            let id = request["id"] ?? 0
            let src = request["src"] as? String ?? "en"
            let dst = request["dst"] as? String ?? "ar"
            if request["status"] != nil {
                reply(["id": id, "status": await engine.status(src, dst)])
                continue
            }
            if request["warm"] != nil {
                do {
                    try await engine.warm(src, dst)
                    reply(["id": id, "ok": true])
                } catch {
                    reply(["id": id, "error": error.localizedDescription])
                }
                continue
            }
            let texts = request["texts"] as? [String] ?? []
            do {
                reply(["id": id, "out": try await engine.translate(src, dst, texts)])
            } catch {
                reply(["id": id, "error": error.localizedDescription])
            }
        }
        #else
        if args.count >= 2, args[1] == "--status" { print("no-os"); return }
        while let line = readLine() {
            let id = (try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any])?["id"] ?? 0
            reply(["id": id, "error": "built without Apple's Translation framework"])
        }
        #endif
    }
}
