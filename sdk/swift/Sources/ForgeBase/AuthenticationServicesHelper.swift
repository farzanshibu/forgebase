import Foundation

#if canImport(AuthenticationServices) && !os(tvOS)
import AuthenticationServices
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

/// `BrowserAuthenticator` backed by `ASWebAuthenticationSession`.
///
/// Register the provider's redirect URI on the server as a custom-scheme URL of your app
/// (`myapp://auth/callback`), pass `callbackURLScheme: "myapp"`, and the system sheet returns the redirect to the app.
@MainActor
public final class WebAuthenticationSessionAuthenticator: NSObject, BrowserAuthenticator {
    /// Share cookies with Safari (default `false` = ephemeral: no "wants to use ... to sign in" prompt reuse, no SSO).
    public var prefersEphemeralSession: Bool
    #if !os(watchOS)
    private let anchor: () -> ASPresentationAnchor
    #endif
    private var current: ASWebAuthenticationSession?

    #if os(watchOS)
    public init(prefersEphemeralSession: Bool = false) {
        self.prefersEphemeralSession = prefersEphemeralSession
        super.init()
    }
    #else
    /// - Parameter anchor: the window the sheet presents from (default: the key window).
    public init(prefersEphemeralSession: Bool = false, anchor: (() -> ASPresentationAnchor)? = nil) {
        self.prefersEphemeralSession = prefersEphemeralSession
        self.anchor = anchor ?? { WebAuthenticationSessionAuthenticator.keyWindow() }
        super.init()
    }
    #endif

    public func authenticate(url: URL, callbackURLScheme: String) async throws -> URL {
        try await run(url: url, scheme: callbackURLScheme)
    }

    private func run(url: URL, scheme: String) async throws -> URL {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<URL, Error>) in
            let session = ASWebAuthenticationSession(url: url, callbackURLScheme: scheme) { callback, error in
                if let callback {
                    cont.resume(returning: callback)
                } else if let e = error as? ASWebAuthenticationSessionError, e.code == .canceledLogin {
                    cont.resume(throwing: ForgeBaseError(kind: .oauth, code: "oauth_cancelled", message: "sign-in was cancelled"))
                } else {
                    cont.resume(
                        throwing: ForgeBaseError(
                            kind: .oauth, code: "oauth_failed", message: error?.localizedDescription ?? "sign-in failed", underlying: error))
                }
            }
            session.prefersEphemeralWebBrowserSession = prefersEphemeralSession
            #if !os(watchOS)
            session.presentationContextProvider = self
            #endif
            current = session
            if !session.start() {
                cont.resume(throwing: ForgeBaseError(kind: .oauth, code: "oauth_failed", message: "could not start the web authentication session"))
            }
        }
    }
}

#if !os(watchOS)
extension WebAuthenticationSessionAuthenticator: ASWebAuthenticationPresentationContextProviding {
    public func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor { anchor() }

    static func keyWindow() -> ASPresentationAnchor {
        #if os(macOS)
        return NSApplication.shared.keyWindow ?? NSApplication.shared.windows.first ?? ASPresentationAnchor()
        #else
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        return scenes.flatMap(\.windows).first(where: \.isKeyWindow) ?? scenes.first?.windows.first ?? ASPresentationAnchor()
        #endif
    }
}
#endif

extension OAuthAPI {
    /// Browser sign-in with `ASWebAuthenticationSession`: opens the provider, waits for the redirect, completes the flow.
    ///
    ///     let result = try await fb.auth.oauth.signInWithBrowser(provider: "github", callbackURLScheme: "myapp")
    @MainActor
    public func signInWithBrowser(
        provider: String, callbackURLScheme: String, options: SignInOptions = .init(), prefersEphemeralSession: Bool = false
    ) async throws -> SignInResult {
        let auth = WebAuthenticationSessionAuthenticator(prefersEphemeralSession: prefersEphemeralSession)
        return try await signIn(provider: provider, options: options, authenticator: auth, callbackURLScheme: callbackURLScheme)
    }
}
#endif
