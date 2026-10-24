import Testing
@testable import HomeSenseCore

@Test func appBuildDisplaysTheSourceCommitAcrossPlatforms() {
    let info: [String: Any] = ["CFBundleShortVersionString": "1.0", "CFBundleVersion": "696",
                              "HomeSenseGitCommit": "abcdef0123456789abcdef0123456789abcdef01",
                              "HomeSenseGitDirty": false]
    let identity = HomeBuildIdentity(info: info)
    #expect(identity.version == "1.0")
    #expect(identity.bundleBuild == "696")
    #expect(identity.displayBuild == "abcdef0")
}

@Test func modifiedSourceCannotLookLikeAnUnmodifiedCommit() {
    let identity = HomeBuildIdentity(info: ["CFBundleVersion": "696",
        "HomeSenseGitCommit": "abcdef0123456789abcdef0123456789abcdef01", "HomeSenseGitDirty": true])
    #expect(identity.displayBuild == "abcdef0 · 未提交改动")
}

@Test func legacyOrInvalidBuildMetadataHasAnHonestFallback() {
    #expect(HomeBuildIdentity(info: ["CFBundleVersion": "36"]).displayBuild == "36")
    #expect(HomeBuildIdentity(info: ["CFBundleVersion": "36", "HomeSenseGitCommit": "not-a-commit"]).sourceCommit == nil)
    #expect(HomeBuildIdentity(info: [:]).displayBuild == "?")
}
