// Sign-in — the OAuth authorization-code flow against WorkOS AuthKit, with
// the secret-bearing exchange delegated to the edge (`POST /auth/exchange`).
// The Cypher app icon on black, one white button — the old mobile app's Gate.
//
// GitHub-only: the authorize `provider` is pinned to the exact `GitHubOAuth`
// (never AuthKit's email/SSO selector) — defense-in-depth, since the dashboard
// AuthKit still exposes email/SSO. Callback and PKCE are unaffected.
//
// Endpoints are fixed to production (the old app's rule: mobile always talks
// to prod; a stale override once broke sign-in in the worst ghost way).

import AuthenticationServices
import SwiftUI

/// Production cloud endpoints — mirrors edge/wrangler.jsonc.
enum Endpoints {
    static let edgeURL = URL(string: "https://edge.letscypher.app")!
    static let workosClientId = "client_01M0JTKFKB6QZWHZDGYW7AN8QH"
    static let workosAPIBase = "https://api.workos.com"
    static let callbackScheme = "cypher"

    static func authorizeURL(state: String, codeChallenge: String) -> URL {
        var components = URLComponents(string: "\(workosAPIBase)/user_management/authorize")!
        components.queryItems = [
            URLQueryItem(name: "response_type", value: "code"),
            URLQueryItem(name: "client_id", value: workosClientId),
            // WorkOS redirects to the edge bridge, which 302s the full query
            // back into the `cypher` scheme for ASWebAuthenticationSession.
            URLQueryItem(name: "redirect_uri", value: "https://edge.letscypher.app/auth/ios/callback"),
            // GitHub-only (see header): pinned to the exact `GitHubOAuth` so
            // the app never surfaces AuthKit's email/SSO screen.
            URLQueryItem(name: "provider", value: "GitHubOAuth"),
            URLQueryItem(name: "state", value: state),
            // RFC 7636: the S256 challenge of the verifier this attempt will
            // present at the exchange; the verifier itself never leaves the
            // device (the edge validates it before it reaches WorkOS).
            URLQueryItem(name: "code_challenge", value: codeChallenge),
            URLQueryItem(name: "code_challenge_method", value: "S256"),
        ]
        return components.url!
    }
}

struct SignInView: View {
    @Environment(AppModel.self) private var model
    @State private var busy = false
    @State private var error: String?
    @State private var authSession = AuthSessionCoordinator()

    var body: some View {
        ZStack {
            Theme.bg.ignoresSafeArea()

            VStack(spacing: 32) {
                Spacer()

                VStack(spacing: 24) {
                    Image("CypherAppIcon")
                        .renderingMode(.original)
                        .resizable()
                        .scaledToFit()
                        .frame(width: 72, height: 72)
                        .accessibilityHidden(true)
                    VStack(spacing: 6) {
                        Text("Cypher")
                            .font(Theme.sans(28, weight: .semibold))
                            .kerning(-0.5)
                            .foregroundStyle(Theme.text)
                        Text("Your coding agents, from anywhere")
                            .font(Theme.sans(15))
                            .foregroundStyle(Theme.textMuted)
                    }
                }

                VStack(spacing: 12) {
                    Button {
                        signIn()
                    } label: {
                        Group {
                            if busy {
                                ProgressView()
                                    .tint(Theme.bg)
                            } else {
                                Text("Log in to Cypher")
                                    .font(Theme.sans(15, weight: .semibold))
                                    .foregroundStyle(Theme.bg)
                            }
                        }
                        .frame(maxWidth: .infinity)
                        .frame(height: 50)
                        .background(Theme.text, in: RoundedRectangle(cornerRadius: 16))
                    }
                    .buttonStyle(.plain)
                    .disabled(busy)
                    .opacity(busy ? 0.6 : 1)

                    if let error {
                        Text(error)
                            .font(Theme.sans(13))
                            .foregroundStyle(Theme.danger)
                            .multilineTextAlignment(.center)
                    }
                }

                Spacer()
            }
            .padding(.horizontal, 32)
            .frame(maxWidth: 480)
        }
    }

    /// The GitHub-only AuthKit code flow: system browser session → edge bridge
    /// (https://edge.letscypher.app/auth/ios/callback) → cypher://callback
    /// with code + state → exchange on the edge. RFC 7636 PKCE: each attempt
    /// draws a fresh verifier, publishes its S256 challenge on the authorize
    /// URL, and presents the verifier exactly once at the exchange — retained
    /// only for this in-flight attempt and released with its completion
    /// closure (cancel/error included).
    private func signIn() {
        busy = true
        error = nil
        let state = UUID().uuidString
        let verifier = PKCE.newVerifier()
        let challenge = PKCE.s256Challenge(for: verifier)
        authSession.start(url: Endpoints.authorizeURL(state: state, codeChallenge: challenge),
                          callbackScheme: Endpoints.callbackScheme) { result in
            Task { @MainActor in
                switch result {
                case .cancelled:
                    busy = false
                case .failure(let message):
                    busy = false
                    error = message
                case .success(let callbackURL):
                    let params = URLComponents(url: callbackURL, resolvingAgainstBaseURL: false)?
                        .queryItems ?? []
                    let code = params.first { $0.name == "code" }?.value
                    let cbState = params.first { $0.name == "state" }?.value
                    // WorkOS redirects back with `error`/`error_description` on
                    // auth failures (e.g. the user declined the login) — surface
                    // that before the generic missing-code error.
                    if let errorDescription = params.first(where: { $0.name == "error_description" })?.value {
                        busy = false
                        error = errorDescription
                        return
                    }
                    guard let code, cbState == state else {
                        busy = false
                        error = "Callback missing code or state mismatch"
                        return
                    }
                    do {
                        try await model.signIn(edgeURL: Endpoints.edgeURL, code: code,
                                               codeVerifier: verifier)
                    } catch {
                        self.error = error.localizedDescription
                    }
                    busy = false
                }
            }
        }
    }
}

// MARK: - Auth session plumbing

/// Wraps ASWebAuthenticationSession with a presentation anchor.
@MainActor
final class AuthSessionCoordinator: NSObject, ASWebAuthenticationPresentationContextProviding {
    enum Outcome {
        case success(URL)
        case cancelled
        case failure(String)
    }

    private var session: ASWebAuthenticationSession?

    func start(url: URL, callbackScheme: String, completion: @escaping (Outcome) -> Void) {
        let session = ASWebAuthenticationSession(url: url,
                                                 callbackURLScheme: callbackScheme) { callbackURL, error in
            if let callbackURL {
                completion(.success(callbackURL))
            } else if let error = error as? ASWebAuthenticationSessionError,
                      error.code == .canceledLogin {
                completion(.cancelled)
            } else {
                completion(.failure(error?.localizedDescription ?? "Sign-in failed"))
            }
        }
        session.presentationContextProvider = self
        session.prefersEphemeralWebBrowserSession = false
        self.session = session
        session.start()
    }

    nonisolated func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        MainActor.assumeIsolated {
            UIApplication.shared.connectedScenes
                .compactMap { ($0 as? UIWindowScene)?.keyWindow }
                .first ?? ASPresentationAnchor()
        }
    }
}

struct OrgPickerView: View {
    @Environment(AppModel.self) private var model
    let tokens: AuthTokens
    let orgs: [AuthOrg]
    @State private var busy = false
    @State private var error: String?
    @State private var newOrgName = ""

    var body: some View {
        ZStack {
            Theme.bg.ignoresSafeArea()
            VStack(spacing: 20) {
                if orgs.isEmpty {
                    onboardingForm
                } else {
                    pickerList
                }
                if let error {
                    Text(error).font(Theme.sans(12)).foregroundStyle(Theme.danger)
                }
                Button("Back") { model.signOut() }
                    .font(Theme.sans(13))
                    .foregroundStyle(Theme.textMuted)
            }
            .padding(24)
            .frame(maxWidth: 480)
        }
    }

    private var pickerList: some View {
        VStack(spacing: 20) {
            Text("Choose an organization")
                .font(Theme.sans(16, weight: .semibold))
                .foregroundStyle(Theme.text)
            VStack(spacing: 8) {
                ForEach(orgs) { org in
                    Button {
                        select(org)
                    } label: {
                        HStack {
                            Text(org.name)
                                .font(Theme.sans(14, weight: .medium))
                                .foregroundStyle(Theme.text)
                            Spacer()
                            Image(systemName: "chevron.right")
                                .font(.system(size: 12))
                                .foregroundStyle(Theme.textFaint)
                        }
                        .padding(.horizontal, 16)
                        .frame(height: 48)
                        .glassEffect(.regular.interactive(), in: RoundedRectangle(cornerRadius: 14))
                    }
                    .disabled(busy)
                }
            }
        }
    }

    /// First-user onboarding: no memberships yet, so ask for the workspace
    /// name (validated locally — trim, 1–80) and create it on submit.
    private var onboardingForm: some View {
        VStack(spacing: 20) {
            Text("Create your workspace")
                .font(Theme.sans(16, weight: .semibold))
                .foregroundStyle(Theme.text)
            VStack(spacing: 8) {
                Text("Your account has no workspaces yet. Name the first one to get started.")
                    .font(Theme.sans(13))
                    .foregroundStyle(Theme.textMuted)
                    .multilineTextAlignment(.center)
                TextField("Workspace name", text: $newOrgName)
                    .font(Theme.sans(14))
                    .foregroundStyle(Theme.text)
                    .textFieldStyle(.plain)
                    .padding(.horizontal, 16)
                    .frame(height: 48)
                    .background(Theme.surface, in: RoundedRectangle(cornerRadius: 14))
                    .overlay(RoundedRectangle(cornerRadius: 14).stroke(Theme.border, lineWidth: 1))
            }
            Button {
                createOrg()
            } label: {
                Group {
                    if busy {
                        ProgressView().tint(Theme.bg)
                    } else {
                        Text("Create workspace")
                            .font(Theme.sans(14, weight: .semibold))
                            .foregroundStyle(Theme.bg)
                    }
                }
                .frame(maxWidth: .infinity)
                .frame(height: 46)
                .background(Theme.text, in: RoundedRectangle(cornerRadius: 14))
            }
            .buttonStyle(.plain)
            .disabled(busy || newOrgName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            .opacity(busy ? 0.6 : 1)
        }
    }

    private func createOrg() {
        guard let name = OrgNameValidator.normalize(newOrgName) else {
            error = "Workspace name must be 1–80 characters"
            return
        }
        busy = true
        error = nil
        Task {
            do {
                try await model.createOrg(name: name, tokens: tokens)
            } catch {
                self.error = error.localizedDescription
            }
            busy = false
        }
    }

    private func select(_ org: AuthOrg) {
        busy = true
        error = nil
        Task {
            do {
                try await model.selectOrg(org, tokens: tokens)
            } catch {
                self.error = error.localizedDescription
            }
            busy = false
        }
    }
}
