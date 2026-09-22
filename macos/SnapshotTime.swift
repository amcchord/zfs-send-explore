import Foundation

enum SnapshotTime {
    static func zoneLabel(_ identifier: String) -> String {
        identifier == "local" ? "Local · \(TimeZone.autoupdatingCurrent.identifier)" : (identifier == "UTC" ? "UTC" : zone(identifier).identifier)
    }
    static func zone(_ identifier: String) -> TimeZone {
        identifier == "local" ? .autoupdatingCurrent : (TimeZone(identifier: identifier) ?? .autoupdatingCurrent)
    }

    static func label(_ timestamp: UInt64?, zone identifier: String, locale: Locale = .autoupdatingCurrent) -> String? {
        guard let timestamp, timestamp > 0, timestamp <= 253_402_300_799 else { return nil }
        let date = Date(timeIntervalSince1970: TimeInterval(timestamp))
        let zone = zone(identifier)
        let formatter = DateFormatter()
        formatter.locale = locale
        formatter.timeZone = zone
        formatter.setLocalizedDateFormatFromTemplate("yMMMdjms")
        let seconds = zone.secondsFromGMT(for: date)
        let offset = String(format: "UTC%@%02d:%02d", seconds < 0 ? "-" : "+", abs(seconds) / 3600, abs(seconds) % 3600 / 60)
        let abbreviation = identifier == "UTC" ? "UTC" : (zone.abbreviation(for: date) ?? zone.identifier)
        return "\(formatter.string(from: date)) \(abbreviation) (\(offset))"
    }
}
