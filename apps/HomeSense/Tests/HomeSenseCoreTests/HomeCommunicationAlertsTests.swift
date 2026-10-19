import XCTest
@testable import HomeSenseCore

final class HomeCommunicationAlertsTests: XCTestCase {
    func testOnlyRepeatedServerFailuresArePresentedAndRecoveryClearsTheMessage() throws {
        func record(_ count: Int) throws -> HomeRecord {
            HomeRecord(try JSONDecoder().decode(HomeJSON.self, from: Data("{\"id\":\"lamp\",\"name\":\"测试灯\",\"communication_error\":{\"code\":\"module_feedback_timeout\",\"consecutive_cycles\":\(count)}}".utf8)))
        }
        XCTAssertNil(HomeCommunicationAlerts.message(for: [try record(2)]))
        XCTAssertTrue(HomeCommunicationAlerts.message(for: [try record(3)])?.contains("测试灯") == true)
        XCTAssertNil(HomeCommunicationAlerts.message(for: [HomeRecord(.object(["id": .string("lamp")]))]))
    }
}
