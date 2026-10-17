import Foundation
import CryptoKit
import OSLog
import Darwin

public enum HomeControlGateReason: String, CaseIterable, Sendable {
    case allowed, disconnected, busy, snapshotNotCurrent, panelMissing, panelInvalid, actionMissing
    case deviceUnavailable, motorNotCalibrated, sceneUnavailable, selectionUnknown, cardActionInvalid
    case unifiedKeypadUnsupported
}

public struct HomeKeypadGateState: Equatable, Sendable {
    var reason: HomeControlGateReason = .allowed
    var connected = false, busy = false, current = false, receivingPush = false
    var configured = false, targetAvailable = false, canControl = false, relativeUnknownAllowed = false
    var selected: Bool?
    public var allowed: Bool { reason == .allowed }
}

/// Local correlation only; the server retains a separate, fresh request_id.
public struct HomeControlOperation: Sendable {
    public let id: UUID
    let started: ContinuousClock.Instant
    let target: String
    let key: Int?
}

enum HomeControlContext {
    @TaskLocal static var operation: HomeControlOperation?
}

enum HomeDiagnosticStage: String, CaseIterable {
    case buttonClick = "button.click", operationBegin = "operation.begin", operationComplete = "operation.complete"
    case gateState = "gate.state", gateCheck = "gate.check", ignored = "operation.ignored", decision = "scene.decision"
    case requestSend = "http.send", response = "http.response", requestFailed = "http.failed", workFailed = "operation.failed"
}

/// Only this allowlisted encoder reaches OSLog, disk, or the export UI. It never
/// accepts headers, complete bodies, URLs, user labels, or error descriptions.
@MainActor public final class HomeDiagnostics {
    public static let subsystem = "com.ngboss.homesense"
    public static let category = "ControlTrace"
    public static let relativeFilePath = "Library/Application Support/HomeSenseDiagnostics/control-trace.jsonl"
    public nonisolated static let maximumFileBytes = 256 * 1024
    public var isEnabled = true
    private let logger = Logger(subsystem: HomeDiagnostics.subsystem, category: HomeDiagnostics.category)
    private let sessionID = UUID()
    private let salt = UUID().uuidString
    private let capacity: Int
    private let emitSystemLog: Bool
    private let file: HomeDiagnosticFile?
    private var lines: [String] = []
    private var storedBytes = 0
    private var sequence: UInt64 = 0
    private var discarded = 0
    public var entryCount: Int { lines.count }

    public nonisolated static var defaultFileURL: URL? {
        guard Bundle.main.bundleIdentifier == "com.ngboss.homesense" else { return nil }
        return FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first?
            .appendingPathComponent("HomeSenseDiagnostics/control-trace.jsonl")
    }
    public init(fileURL: URL? = HomeDiagnostics.defaultFileURL, capacity: Int = 512, emitSystemLog: Bool = true) {
        self.capacity = min(1024, max(1, capacity)); self.emitSystemLog = emitSystemLog
        file = fileURL.map(HomeDiagnosticFile.init)
    }
    func operation(target: String, key: Int? = nil) -> HomeControlOperation {
        let alias = SHA256.hash(data: Data((salt + target).utf8)).prefix(8).map { String(format: "%02x", $0) }.joined()
        return HomeControlOperation(id: UUID(), started: .now, target: alias, key: key.flatMap { (1...6).contains($0) ? $0 : nil })
    }
    func record(_ stage: HomeDiagnosticStage, operation explicit: HomeControlOperation? = nil,
                gate: HomeKeypadGateState? = nil, on: Bool? = nil, method: String? = nil,
                path: String? = nil, requestID: String? = nil, status: Int? = nil,
                responseData: Data? = nil, error: Error? = nil, reason: HomeControlGateReason? = nil) {
        guard isEnabled else { return }
        sequence &+= 1
        let operation = explicit ?? HomeControlContext.operation
        var fields: [String: HomeJSON] = ["event": .string(stage.rawValue), "session": .string(sessionID.uuidString), "sequence": .unsigned(sequence),
            "time": .string(ISO8601DateFormatter().string(from: Date()))]
        if let operation {
            let duration = operation.started.duration(to: .now).components
            if stage != .gateState {
                fields["operation_id"] = .string(operation.id.uuidString)
                fields["elapsed_ms"] = .integer(max(0, duration.seconds * 1000 + duration.attoseconds / 1_000_000_000_000_000))
            }
            fields["target_alias"] = .string(operation.target)
            if let key = operation.key { fields["key"] = .integer(Int64(key)) }
        }
        if let gate {
            fields["gate"] = .string(gate.reason.rawValue); fields["allowed"] = .bool(gate.allowed)
            fields["connected"] = .bool(gate.connected); fields["busy"] = .bool(gate.busy)
            fields["current"] = .bool(gate.current); fields["receiving_push"] = .bool(gate.receivingPush)
            fields["configured"] = .bool(gate.configured); fields["target_available"] = .bool(gate.targetAvailable)
            fields["can_control"] = .bool(gate.canControl); fields["relative_unknown_allowed"] = .bool(gate.relativeUnknownAllowed)
            fields["selected"] = gate.selected.map(HomeJSON.bool) ?? .null
        }
        if let reason { fields["reason"] = .string(reason.rawValue) }
        if let on { fields["on"] = .bool(on) }
        if let method { fields["method"] = .string(["GET","POST","PUT","DELETE","PATCH"].contains(method) ? method : "other") }
        if let path { fields["path"] = .string(Self.safePath(path)) }
        if let id = requestID.flatMap(UUID.init(uuidString:)) { fields["request_id"] = .string(id.uuidString) }
        if let status { fields["http_status"] = .integer(Int64(status)) }
        if let responseData {
            fields["response_bytes"] = .integer(Int64(responseData.count))
            if let body = try? JSONDecoder().decode(HomeJSON.self, from: responseData) {
                if let code = body["error"].text { fields["error_code"] = .string(Self.safeServerCode(code)) }
                if let outcome = body["outcome"].text ?? body["status"].text {
                    fields["outcome"] = .string(["accepted","unknown","pending","rejected","failed","completed","unchanged","cancelled"].contains(outcome) ? outcome : "other")
                }
            }
        }
        if let error {
            let ns = error as NSError
            fields["error_domain"] = .string([NSURLErrorDomain, NSCocoaErrorDomain, NSPOSIXErrorDomain].contains(ns.domain) ? ns.domain : "other")
            fields["error_number"] = .integer(Int64(ns.code))
            fields["cancelled"] = .bool(Task.isCancelled || error is CancellationError || (error as? URLError)?.code == .cancelled)
        }
        guard let data = try? HomeJSON.object(fields).data(), let line = String(data: data, encoding: .utf8) else { return }
        lines.append(line); storedBytes += data.count + 1
        while lines.count > capacity || storedBytes > Self.maximumFileBytes - 1024 {
            storedBytes -= lines.removeFirst().utf8.count + 1; discarded += 1
        }
        if emitSystemLog { logger.info("\(line, privacy: .public)") }
        file?.submit(exportText())
    }
    /// The two bounded sessions are concatenated as JSONL, each with its header.
    /// Reads only our known previous snapshot, never any other container data.
    public func exportIncludingPrevious() async -> String {
        let current = exportText()
        return (await file?.previousSnapshot() ?? "") + current
    }
    public func exportText() -> String {
        let build = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? ""
        let safeBuild = !build.isEmpty && build.count <= 12 && build.allSatisfy(\.isNumber) ? build : "unknown"
        let header: HomeJSON = .object(["format": .string("homesense-control-trace-v1"), "session": .string(sessionID.uuidString),
            "build": .string(safeBuild), "discarded": .integer(Int64(discarded)), "enabled": .bool(isEnabled)])
        return ((try? String(data: header.data(), encoding: .utf8)) ?? "{}") + "\n" + lines.joined(separator: "\n") + "\n"
    }
    fileprivate nonisolated static func safePath(_ path: String) -> String {
        let fixed: Set<String> = ["/api/v1/session","/api/v1/devices","/api/v1/scenes","/api/v1/rooms","/api/v1/device-commands",
            "/api/v1/scene-actions","/api/v1/scene-relative-actions","/api/v1/away-off","/api/v1/keypad-settings","/api/v1/keypad-actions","/api/v1/config/versions","/api/v1/config/drafts"]
        if fixed.contains(path) { return path }
        if path.hasPrefix("/api/v1/config/") { return "/api/v1/config/*" }
        return "/api/v1/other"
    }
    fileprivate nonisolated static func safeServerCode(_ code: String) -> String {
        let allowed: Set<String> = [
            "administrator_required", "ambiguous_credentials", "authentication_required", "backup_catalog_unavailable",
            "backup_context_unavailable", "backup_inspection_failed", "backups_unavailable", "calendar_diagnostics_unavailable",
            "configuration_changed", "configuration_changed_or_unavailable", "configuration_management_unavailable", "control_failed",
            "credential_request_too_large", "credential_store_unavailable", "device_not_permitted", "device_snapshot_unavailable",
            "diagnostics_unavailable", "draft_invalid_conflicting_or_unavailable", "draft_not_found", "draft_storage_unavailable",
            "edit_too_large", "forbidden", "idempotency_conflict", "insufficient_privileges",
            "invalid_backup_selection", "invalid_body", "invalid_brightness", "invalid_credential_id",
            "invalid_credential_request", "invalid_device", "invalid_device_binding", "invalid_device_edit",
            "invalid_device_scope", "invalid_draft_id", "invalid_host", "invalid_module_binding",
            "invalid_module_calibration", "invalid_module_edit", "invalid_module_migration", "invalid_module_query",
            "invalid_module_replacement", "invalid_motor_calibration", "invalid_origin", "invalid_query_receipt",
            "invalid_request", "invalid_request_id", "invalid_room_edit", "invalid_run",
            "invalid_stored_draft", "keypad_settings_not_permitted_or_incompatible", "method_not_allowed", "module_channel_unavailable",
            "keypad_unavailable", "keypad_action_unavailable", "keypad_scope_denied", "keypad_led_mapping_missing", "execution_outcome_unknown",
            "module_inventory_unavailable", "module_query_unsupported_or_unavailable", "module_revision_changed", "native_app_required",
            "not_found", "origin_required", "owned_query_not_found", "owned_start_not_found",
            "panel_credential_required", "persistence_unavailable", "query_not_in_current_runtime", "query_not_supported",
            "query_receipt_unknown", "query_scope_unavailable", "receipt_unavailable_outcome_unknown", "relative_request_not_found",
            "request_failed", "room_in_use", "run_not_found", "run_summary_unavailable",
            "runtime_not_ready", "runtime_reconfiguring", "runtime_unavailable", "scene_availability_unavailable",
            "scene_definition_unavailable", "scene_not_permitted", "scene_selection_unavailable", "scene_tracking_unavailable",
            "selection_too_large", "source_credential_required", "source_scope_changed", "stale_revision",
            "start_not_in_current_runtime", "unauthorized", "unexpected_body",
        ]
        return allowed.contains(code) ? code : "other"
    }
}

/// At most one pending complete ring snapshot. Disk I/O never blocks admission,
/// changes control results, or creates a network request. Atomic replacement
/// lets a targeted device-file copy read either complete snapshot.
private final class HomeDiagnosticFile: Sendable {
    private let input: AsyncStream<String>.Continuation
    private let writer: Task<Void, Never>
    private let url: URL
    init(url: URL) {
        self.url = url
        let (stream, continuation) = AsyncStream<String>.makeStream(bufferingPolicy: .bufferingNewest(1))
        input = continuation
        writer = Task.detached(priority: .utility) {
            var rotated = false
            for await text in stream {
                guard !Task.isCancelled else { return }
                do {
                    if !rotated { try Self.rotate(url); rotated = true }
                    try Self.write(text, to: url)
                } catch { /* Diagnostics must never fail a control. */ }
            }
        }
    }
    deinit { input.finish() }
    func submit(_ text: String) { input.yield(text) }
    func previousSnapshot() async -> String {
        let previous = url.deletingLastPathComponent().appendingPathComponent("control-trace.previous.jsonl")
        return await Task.detached(priority: .utility) {
            guard let size = try? previous.resourceValues(forKeys: [.fileSizeKey]).fileSize,
                  size <= HomeDiagnostics.maximumFileBytes,
                  let data = try? Data(contentsOf: previous), let text = String(data: data, encoding: .utf8),
                  Self.validSnapshot(text) else { return "" }
            return text
        }.value
    }
    private static func rotate(_ url: URL) throws {
        let manager = FileManager.default
        let previous = url.deletingLastPathComponent().appendingPathComponent("control-trace.previous.jsonl")
        if manager.fileExists(atPath: url.path) {
            if manager.fileExists(atPath: previous.path) { try manager.removeItem(at: previous) }
            if (try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? Int.max) <= HomeDiagnostics.maximumFileBytes {
                try manager.moveItem(at: url, to: previous)
            } else { try manager.removeItem(at: url) }
        }
    }
    private static func validSnapshot(_ text: String) -> Bool {
        let rawLines = text.split(separator: "\n")
        let rows = rawLines.compactMap { try? JSONDecoder().decode(HomeJSON.self, from: Data($0.utf8)) }
        guard rows.count == rawLines.count else { return false }
        guard let header = rows.first, header["format"].text == "homesense-control-trace-v1",
              header["session"].text.flatMap(UUID.init(uuidString:)) != nil,
              rows.count <= 1025 else { return false }
        let booleans: Set<String> = ["allowed","connected","busy","current","receiving_push","configured","target_available","can_control","relative_unknown_allowed","on","cancelled"]
        let integers: Set<String> = ["elapsed_ms","sequence","http_status","response_bytes","error_number"]
        for row in rows.dropFirst() {
            guard case .object(let fields) = row else { return false }
            for (key, value) in fields {
                switch key {
                case "event": guard HomeDiagnosticStage.allCases.contains(where: { $0.rawValue == value.text }) else { return false }
                case "session","operation_id","request_id": guard value.text.flatMap(UUID.init(uuidString:)) != nil else { return false }
                case "time": guard let time = value.text, ISO8601DateFormatter().date(from: time) != nil else { return false }
                case "target_alias": guard let alias = value.text, alias.count == 16, alias.allSatisfy({ "0123456789abcdef".contains($0) }) else { return false }
                case "key": guard let number = value.integerText.flatMap(Int.init), (1...6).contains(number) else { return false }
                case "gate","reason": guard HomeControlGateReason.allCases.contains(where: { $0.rawValue == value.text }) else { return false }
                case "selected": if value != .null { guard case .bool = value else { return false } }
                case "method": guard ["GET","POST","PUT","DELETE","PATCH","other"].contains(value.text ?? "") else { return false }
                case "path": guard let path = value.text, HomeDiagnostics.safePath(path) == path else { return false }
                case "error_code": guard let code = value.text, code == "other" || HomeDiagnostics.safeServerCode(code) == code else { return false }
                case "outcome": guard ["accepted","unknown","pending","rejected","failed","completed","unchanged","cancelled","other"].contains(value.text ?? "") else { return false }
                case "error_domain": guard [NSURLErrorDomain, NSCocoaErrorDomain, NSPOSIXErrorDomain,"other"].contains(value.text ?? "") else { return false }
                default:
                    if booleans.contains(key) { guard case .bool = value else { return false } }
                    else if integers.contains(key) { guard value.integerText.flatMap(Int64.init) != nil else { return false } }
                    else { return false }
                }
            }
        }
        // Re-encode the header's fixed fields rather than trusting its extra keys.
        guard case .object(let fields) = header, Set(fields.keys) == ["format","session","build","discarded","enabled"],
              let build = header["build"].text, build == "unknown" || (!build.isEmpty && build.count <= 12 && build.allSatisfy(\.isNumber)),
              header["discarded"].integerText.flatMap(Int64.init) != nil, case .bool = header["enabled"] else { return false }
        return true
    }
    private static func write(_ text: String, to url: URL) throws {
        guard text.utf8.count <= HomeDiagnostics.maximumFileBytes else { return }
        let manager = FileManager.default, directory = url.deletingLastPathComponent()
        try manager.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        var values = URLResourceValues(); values.isExcludedFromBackup = true
        var dir = directory; try dir.setResourceValues(values)
        let temporary = directory.appendingPathComponent("control-trace.pending")
        var attributes: [FileAttributeKey: Any] = [.posixPermissions: 0o600]
        #if os(iOS)
        attributes[.protectionKey] = FileProtectionType.completeUntilFirstUserAuthentication
        #endif
        guard manager.createFile(atPath: temporary.path, contents: nil, attributes: attributes) else { return }
        defer { try? manager.removeItem(at: temporary) }
        let handle = try FileHandle(forWritingTo: temporary)
        do { try handle.write(contentsOf: Data(text.utf8)); try handle.close() }
        catch { try? handle.close(); throw error }
        guard rename(temporary.path, url.path) == 0 else { throw POSIXError(.EIO) }
    }
}
