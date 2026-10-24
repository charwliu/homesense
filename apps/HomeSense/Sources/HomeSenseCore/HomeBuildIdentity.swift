import Foundation

public struct HomeBuildIdentity: Sendable {
    public let version: String
    public let bundleBuild: String
    public let sourceCommit: String?
    public let sourceIsDirty: Bool

    public init(info: [String: Any]) {
        version = info["CFBundleShortVersionString"] as? String ?? "?"
        bundleBuild = info["CFBundleVersion"] as? String ?? "?"
        let commit = info["HomeSenseGitCommit"] as? String ?? ""
        sourceCommit = commit.count == 40 && commit.allSatisfy { "0123456789abcdef".contains($0) }
            ? commit : nil
        sourceIsDirty = info["HomeSenseGitDirty"] as? Bool ?? false
    }

    public var displayBuild: String {
        let revision = sourceCommit.map { String($0.prefix(7)) } ?? bundleBuild
        return revision + (sourceIsDirty ? " · 未提交改动" : "")
    }
}
