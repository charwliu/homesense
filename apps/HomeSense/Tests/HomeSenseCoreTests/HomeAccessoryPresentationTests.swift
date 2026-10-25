import Foundation
import Testing
@testable import HomeSenseCore

@Suite struct HomeAccessoryPresentationTests {
    @Test func binaryFanHasPowerFeedbackWithoutTemperatureOrBrightness() {
        let on = HomeAccessoryPresentation(kind: .fan, value: .boolean(true), brightness: 100)
        #expect(on.isActive && on.status == "开启" && on.on == true)
        #expect(HomeAccessoryKind.fan.symbol == "fan.fill")
        #expect(HomeAccessoryPresentation(kind: .fan, value: .boolean(false)).status == "关闭")
        #expect(HomeAccessoryPresentation(kind: .fan, value: nil).status == "状态未知")
        #expect(HomeAccessoryPresentation(kind: .fan, value: .boolean(true), reachable: false).status == "未响应")
    }
    @Test func rememberedBrightnessCannotTurnAnOffOrUnknownLightOn() {
        let off = HomeAccessoryPresentation(kind: .light, value: .boolean(false), brightness: 100)
        #expect(off.status == "关闭" && !off.isActive)
        let unknown = HomeAccessoryPresentation(kind: .light, value: nil, brightness: 100)
        #expect(unknown.status == "状态未知" && !unknown.isActive && unknown.on == nil)
        let on = HomeAccessoryPresentation(kind: .light, value: .boolean(true), brightness: 42)
        #expect(on.status == "42%" && on.isActive)
    }
    @Test(arguments: [0.0, 50, 100]) func curtainUsesObservedPosition(_ position: Double) {
        let value = HomeAccessoryPresentation(kind: .curtain, value: .number(position))
        #expect(value.status == (position == 0 ? "已关闭" : position == 100 ? "已打开" : "打开 50%"))
        #expect(value.isActive == (position > 0))
    }
    @Test func offlineAndInvalidPositionsStayUnknown() {
        #expect(HomeAccessoryPresentation(kind: .curtain, value: .number(125)).status == "状态未知")
        #expect(HomeAccessoryPresentation(kind: .curtain, value: .number(.nan)).status == "状态未知")
        let offline = HomeAccessoryPresentation(kind: .light, value: .boolean(true), reachable: false)
        #expect(offline.status == "未响应" && !offline.isActive && offline.on == nil)
    }
    @Test func sensorsRetainTheirOwnMeaning() {
        #expect(HomeAccessoryPresentation(kind: .motion, value: .number(0)).status == "未检测到移动")
        #expect(HomeAccessoryPresentation(kind: .motion, value: .boolean(true)).status == "检测到移动")
        #expect(HomeAccessoryPresentation(kind: .contact, value: .number(1)).status == "打开")
        #expect(HomeAccessoryPresentation(kind: .humidity, value: .number(51)).status == "51%")
    }
    @Test func summaryNeverCallsUnknownLightsOff() {
        let off = HomeAccessoryPresentation(kind: .light, value: .boolean(false))
        let unknown = HomeAccessoryPresentation(kind: .light, value: nil)
        let on = HomeAccessoryPresentation(kind: .light, value: .boolean(true))
        #expect(HomeAccessoryPresentation.lightSummary([off]) == "全部关闭")
        #expect(HomeAccessoryPresentation.lightSummary([off, unknown]) == "1 盏状态未知")
        #expect(HomeAccessoryPresentation.lightSummary([on, unknown]) == "1 盏开启 · 1 未知")
    }
}
