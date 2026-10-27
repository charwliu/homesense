import Foundation

public struct HomeHTTPResponse: Sendable {
    public let data: Data
    public let status: Int
    public init(data: Data, status: Int) { self.data = data; self.status = status }
}
public protocol HomeHTTPTransport: Sendable {
    func send(_ request: URLRequest) async throws -> HomeHTTPResponse
}

final class NoRedirects: NSObject, URLSessionTaskDelegate, Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

public final class HomeURLSessionTransport: HomeHTTPTransport, Sendable {
    private let session: URLSession
    public convenience init() { self.init(delegate: NoRedirects()) }
    // Internal test injection, matching HomeWebSocketTransport. The public
    // initializer keeps normal system trust and the no-redirect boundary.
    init(delegate: any URLSessionDelegate & Sendable) {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpShouldSetCookies = false
        configuration.httpCookieStorage = nil
        configuration.urlCache = nil
        configuration.timeoutIntervalForRequest = 12
        configuration.timeoutIntervalForResource = 20
        session = URLSession(configuration: configuration, delegate: delegate, delegateQueue: nil)
    }
    public func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse, data.count <= Self.responseLimit(for: request) else {
            throw HomeServerError.invalidResponse
        }
        return HomeHTTPResponse(data: data, status: response.statusCode)
    }
    private static func responseLimit(for request: URLRequest) -> Int {
        guard (request.httpMethod ?? "GET") == "GET", let path = request.url?.path else { return 2 * 1024 * 1024 }
        let parts = path.split(separator: "/").map(String.init)
        guard parts.count >= 5, Array(parts.prefix(3)) == ["api", "v1", "config"] else { return 2 * 1024 * 1024 }
        let draft = parts[3] == "drafts" && UUID(uuidString: parts[4]) != nil
            && (parts.count == 5 || parts.count == 6 && parts[5] == "source")
        let exported = parts.count == 6 && parts[3] == "versions" && parts[5] == "export"
            && UInt64(parts[4]).map { $0 > 0 && String($0) == parts[4] } == true
        // Source responses escape complete JSON documents inside JSON strings.
        // Other HTTP responses and all WebSocket frames retain their old caps.
        return draft || exported ? 8 * 1024 * 1024 + 4096 : 2 * 1024 * 1024
    }
}

public struct HomeServerEndpoint: Equatable, Sendable {
    public let origin: String
    public init(_ text: String) throws {
        let value = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard var components = URLComponents(string: value), components.scheme?.lowercased() == "https",
              let host = components.host, !host.isEmpty,
              components.user == nil, components.password == nil, components.query == nil,
              components.fragment == nil, components.path.isEmpty || components.path == "/",
              components.port.map({ (1...65535).contains($0) }) ?? true else {
            throw HomeServerError.invalidEndpoint
        }
        components.scheme = "https"; components.host = host.lowercased(); components.path = ""
        if components.port == 443 { components.port = nil }
        guard let origin = components.url?.absoluteString else { throw HomeServerError.invalidEndpoint }
        self.origin = origin
    }
    public func request(path: String, method: String, token: String, body: HomeJSON?) throws -> URLRequest {
        guard path.hasPrefix("/api/v1/"), let url = URL(string: origin + path) else { throw HomeServerError.invalidEndpoint }
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData)
        request.httpMethod = method
        request.setValue(origin.replacingOccurrences(of: "https://", with: ""), forHTTPHeaderField: "Host")
        request.setValue(origin, forHTTPHeaderField: "Origin")
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.httpShouldHandleCookies = false
        if let body {
            request.httpBody = try body.data()
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        if method != "GET", path.hasPrefix("/api/v1/config/drafts/") || path.hasSuffix("/restore-draft") {
            request.setValue(UUID().uuidString, forHTTPHeaderField: "Idempotency-Key")
        }
        return request
    }
    public func eventRequest(token: String, includeModules: Bool = false) throws -> URLRequest {
        var request = try request(path: "/api/v1/events", method: "GET", token: token, body: nil)
        var url = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!
        if includeModules { request.setValue("1", forHTTPHeaderField: "X-HomeSense-Module-Events") }
        url.scheme = "wss"
        request.url = url.url
        return request
    }
}
