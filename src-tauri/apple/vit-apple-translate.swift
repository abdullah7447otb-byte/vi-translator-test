// The app's helper for Apple's on-device translation (macOS 26 and later): newline-delimited JSON on
// stdin/stdout, one request a line, answered in order.
//
//   {"id": 1, "src": "en", "dst": "ar", "texts": ["…", …]}  ->  {"id": 1, "out": ["…", …]}
//   {"id": 2, "status": true, "src": "en", "dst": "ar"}     ->  {"id": 2, "status": "installed" | "supported" | "unsupported"}
//   anything wrong                                         ->  {"id": n, "error": "…"}
//
// `vit-apple-translate --status en ar` prints the status alone and exits.
//
// One session per language pair, reused; requests one at a time (no concurrent use of a session). The
// handling follows CouchSubs' Translator.swift (AGPL-3.0, (C) 2026 Sinan Yorulmaz, commit bc54ff7:
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

    func translate(_ src: String, _ dst: String, _ texts: [String]) async throws -> [String] {
        let key = src + ">" + dst
        if sessions[key] == nil {
            let st = await status(src, dst)
            guard st == "installed" else {
                throw NSError(domain: "vit", code: 1, userInfo: [NSLocalizedDescriptionKey: st == "supported" ? "the language pack isn't downloaded" : "this language pair isn't supported"])
            }
            sessions[key] = TranslationSession(installedSource: Locale.Language(identifier: src), target: Locale.Language(identifier: dst))
        }
        guard let session = sessions[key] else { return texts }
        var out: [String] = []
        for text in texts {
            if text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                out.append("")
                continue
            }
            out.append(try await session.translate(text).targetText)
        }
        return out
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
