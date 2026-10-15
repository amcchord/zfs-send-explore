import Foundation

@main struct SnapshotTimeTests {
    static func main() {
        func label(_ iso: String, _ zone: String) -> String {
            let date = ISO8601DateFormatter().date(from: iso)!
            return SnapshotTime.label(UInt64(date.timeIntervalSince1970), zone: zone, locale: Locale(identifier: "en_US"))!
        }
        let first = label("2026-11-01T05:30:00Z", "America/New_York")
        let second = label("2026-11-01T06:30:00Z", "America/New_York")
        assert(first.contains("1:30:00") && first.contains("UTC-04:00"), first)
        assert(second.contains("1:30:00") && second.contains("UTC-05:00"), second)
        assert(first != second)
        let previousDay = label("2026-09-22T00:15:00Z", "America/Los_Angeles")
        assert(previousDay.contains("Sep 21, 2026"), previousDay)
        assert(label("2026-09-22T00:15:00Z", "Asia/Kathmandu").contains("UTC+05:45"))
        assert(label("2026-09-22T00:15:00Z", "UTC").contains("UTC+00:00"))
        assert(SnapshotTime.label(nil, zone: "UTC") == nil)
        assert(SnapshotTime.label(0, zone: "UTC") == nil)
        assert(SnapshotTime.label(UInt64.max, zone: "UTC") == nil)
        assert(SnapshotTime.zone("local") == .autoupdatingCurrent)
        assert(SnapshotTime.zone("Removed/Zone") == .autoupdatingCurrent)
        let short = SnapshotTime.short(1_758_477_600, zone: "UTC", locale: Locale(identifier: "en_US"))!
        assert(short.contains("2025") && short.contains("6:00"), short)
        let today = SnapshotTime.short(UInt64(Date().timeIntervalSince1970), zone: "local", locale: Locale(identifier: "en_US"))!
        assert(today.hasPrefix("Today"), today)
        assert(SnapshotTime.short(nil, zone: "UTC") == nil)
        assert(SnapshotTime.zoneName("UTC") == "UTC")
        assert(SnapshotTime.zoneName("America/New_York") == "New York time")
        print("Snapshot date tests passed: DST, date boundaries, fractional offsets, local default and missing timestamps.")
    }
}
