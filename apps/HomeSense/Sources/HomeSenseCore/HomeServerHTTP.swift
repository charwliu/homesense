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
    public init() {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpShouldSetCookies = false
        configuration.httpCookieStorage = nil
        configuration.urlCache = nil
        configuration.timeoutIntervalForRequest = 12
        configuration.timeoutIntervalForResource = 20
        session = URLSession(configuration: configuration, delegate: NoRedirects(), delegateQueue: nil)
    }
    public func send(_ request: URLRequest) async throws -> HomeHTTPResponse {
        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse, data.count <= 2 * 1024 * 1024 else {
            throw HomeServerError.invalidResponse
        }
        return HomeHTTPResponse(data: data, status: response.statusCode)
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
    public func eventRequest(token: String) throws -> URLRequest {
        var request = try request(path: "/api/v1/events", method: "GET", token: token, body: nil)
        var url = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!
        url.scheme = "wss"
        request.url = url.url
        return request
    }
}
