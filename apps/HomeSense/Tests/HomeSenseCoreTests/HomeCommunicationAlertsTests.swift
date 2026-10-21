import XCTest
@testable import HomeSenseCore

final class HomeCommunicationAlertsTests: XCTestCase {
    func testHouseholdFailureUsesLatestServerReceiptWithoutRequestingRetry() {
        let failed: HomeJSON = .object(["state": .string("failed"), "current_module": .string("bus/04")])
        let pending: HomeJSON = .object(["state": .string("pending")])
        XCTAssertTrue(HomeCommunicationAlerts.householdMessage(for: .array([failed]))?.contains("bus/04") == true)
        XCTAssertTrue(HomeCommunicationAlerts.householdMessage(for: .array([failed]))?.contains("不会自动重复执行") == true)
        XCTAssertNil(HomeCommunicationAlerts.householdMessage(for: .array([pending, failed])))
        XCTAssertNil(HomeCommunicationAlerts.householdMessage(for: .array([.object(["state": .string("confirmed")]), failed])))
        XCTAssertNil(HomeCommunicationAlerts.householdMessage(for: .null))
    }
    func testOnlyRepeatedServerFailuresArePresentedAndRecoveryClearsTheMessage() throws {
        func record(_ count: Int) throws -> HomeRecord {
            HomeRecord(try JSONDecoder().decode(HomeJSON.self, from: Data("{\"id\":\"lamp\",\"name\":\"测试灯\",\"communication_error\":{\"code\":\"module_feedback_timeout\",\"consecutive_cycles\":\(count)}}".utf8)))
        }
        XCTAssertNil(HomeCommunicationAlerts.message(for: [try record(2)]))
        XCTAssertTrue(HomeCommunicationAlerts.message(for: [try record(3)])?.contains("测试灯") == true)
        XCTAssertNil(HomeCommunicationAlerts.message(for: [HomeRecord(.object(["id": .string("lamp")]))]))
    }
}
