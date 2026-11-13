// A small command-line tour of the SDK against a real server.
//
//   FORGEBASE_URL=http://localhost:8080 FORGEBASE_API_KEY=fb_... swift run ForgeBaseExample health
//   swift run ForgeBaseExample list posts
//   swift run ForgeBaseExample create posts '{"title":"Hello"}'
//   swift run ForgeBaseExample put avatars me.png ./me.png
//   swift run ForgeBaseExample watch posts        # prints record events until you press Ctrl-C
//
// Sign in as a user instead of using a key: FORGEBASE_EMAIL / FORGEBASE_PASSWORD.
import Foundation
import ForgeBase

@main
struct Example {
    static func main() async {
        do {
            try await run(Array(CommandLine.arguments.dropFirst()))
        } catch let e as ForgeBaseError {
            FileHandle.standardError.write(Data("error [\(e.kind), \(e.status) \(e.code)]: \(e.message)\n".utf8))
            exit(1)
        } catch {
            FileHandle.standardError.write(Data("error: \(error)\n".utf8))
            exit(1)
        }
    }

    static func run(_ args: [String]) async throws {
        let env = ProcessInfo.processInfo.environment
        guard let url = URL(string: env["FORGEBASE_URL"] ?? "http://localhost:8080") else { throw ForgeBaseError.invalidRequestForExample("bad FORGEBASE_URL") }
        let fb = ForgeBaseClient(baseURL: url, apiKey: env["FORGEBASE_API_KEY"])
        if env["FORGEBASE_API_KEY"] == nil, let email = env["FORGEBASE_EMAIL"], let password = env["FORGEBASE_PASSWORD"] {
            let result = try await fb.auth.signIn(email: email, password: password)
            guard result.session != nil else {
                print("a second factor is required: use fb.auth.verifyMFA(mfaToken:code:)")
                return
            }
        }

        switch args.first {
        case "health":
            print(await fb.health() ? "up" : "down")
            print(try await fb.systemInfo())

        case "list" where args.count >= 2:
            let page = try await fb.collection(args[1]).list(ListOptions(sort: ["-created"], perPage: 20))
            for record in page.items { print(record) }
            print("\(page.items.count) of \(page.totalItems ?? page.items.count)")

        case "create" where args.count >= 3:
            let body = try JSON(data: Data(args[2].utf8))
            print(try await fb.collection(args[1]).create(body))

        case "put" where args.count >= 4:
            let object = try await fb.storage.from(args[1]).put(args[2], fileURL: URL(fileURLWithPath: args[3]))
            print("stored \(object.key) (\(object.size) bytes, version \(object.versionId))")

        case "watch" where args.count >= 2:
            let sub = try await fb.realtime.subscribe(.collection(args[1]))
            print("watching \(args[1]) ... Ctrl-C to stop")
            for await event in sub.events {
                switch event {
                case .record(let e): print("\(e.kind.rawValue) \(e.recordId): \(e.data)")
                case .reset(let r): print("reset (\(r.reason)): reload the collection, then continue")
                default: break
                }
            }

        default:
            print(
                """
                usage: ForgeBaseExample <command>
                  health
                  list <collection>
                  create <collection> '<json>'
                  put <bucket> <key> <file>
                  watch <collection>
                """)
        }
    }
}

extension ForgeBaseError {
    fileprivate static func invalidRequestForExample(_ m: String) -> ForgeBaseError {
        ForgeBaseError(kind: .invalidRequest, code: "invalid_request", message: m)
    }
}
