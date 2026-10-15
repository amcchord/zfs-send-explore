import Foundation

enum SnapshotTime {
    static func zoneLabel(_ identifier: String) -> String {
        identifier == "local" ? "Local · \(TimeZone.autoupdatingCurrent.identifier)" : (identifier == "UTC" ? "UTC" : zone(identifier).identifier)
    }
    static func zone(_ identifier: String) -> TimeZone {
        identifier == "local" ? .autoupdatingCurrent : (TimeZone(identifier: identifier) ?? .autoupdatingCurrent)
    }

    /// Readable city or zone name for sentences such as "Times shown in New York time".
    static func zoneName(_ identifier: String) -> String {
        if identifier == "UTC" { return "UTC" }
        let zone = zone(identifier)
        let city = zone.identifier.split(separator: "/").last.map { $0.replacingOccurrences(of: "_", with: " ") } ?? zone.identifier
        return identifier == "local" ? "your Mac’s time (\(city))" : "\(city) time"
    }

    static func date(_ timestamp: UInt64?) -> Date? {
        guard let timestamp, timestamp > 0, timestamp <= 253_402_300_799 else { return nil }
        return Date(timeIntervalSince1970: TimeInterval(timestamp))
    }

    /// Precise form for details and tooltips: seconds plus the UTC offset, so
    /// repeated daylight-saving hours remain distinguishable.
    static func label(_ timestamp: UInt64?, zone identifier: String, locale: Locale = .autoupdatingCurrent) -> String? {
        guard let date = date(timestamp) else { return nil }
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

    /// Compact form for tight spaces: "Today, 2:00 PM" or "Sep 21, 2:00 PM" (year only when not this year).
    static func compact(_ timestamp: UInt64?, zone identifier: String, locale: Locale = .autoupdatingCurrent, now: Date = Date()) -> String? {
        guard let date = date(timestamp) else { return nil }
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = zone(identifier)
        let formatter = DateFormatter()
        formatter.locale = locale
        formatter.timeZone = calendar.timeZone
        if calendar.isDate(date, inSameDayAs: now) || calendar.isDate(date, inSameDayAs: now.addingTimeInterval(-86_400)) {
            formatter.dateStyle = .short; formatter.timeStyle = .short; formatter.doesRelativeDateFormatting = true
        } else {
            formatter.setLocalizedDateFormatFromTemplate(calendar.isDate(date, equalTo: now, toGranularity: .year) ? "MMMdjmm" : "yMMMdjmm")
        }
        return formatter.string(from: date)
    }

    /// Short form for lists: "Today at 2:00 PM", "Yesterday at 9:15 AM" or "Sep 21, 2026 at 2:00 PM".
    static func short(_ timestamp: UInt64?, zone identifier: String, locale: Locale = .autoupdatingCurrent) -> String? {
        guard let date = date(timestamp) else { return nil }
        let formatter = DateFormatter()
        formatter.locale = locale
        formatter.timeZone = zone(identifier)
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        formatter.doesRelativeDateFormatting = true
        return formatter.string(from: date)
    }
}
