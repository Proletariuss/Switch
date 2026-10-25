import Foundation

// Frozen comparison from APIClient.swift SHA-256
// 4754f8cbcee36acf4908b7db3bec4bbeb239020d09f2f60b26ed480ad1ada23a.
// This intentionally retains the old per-date formatter construction. Candidate
// tests must call APIClient.decodeResponse, never a second optimized decoder.
enum BaselineAPIJSONDecoder {
    static func makeDecoder() -> JSONDecoder {
        let result = JSONDecoder()
        result.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let str = try container.decode(String.self)
            let withFraction = ISO8601DateFormatter()
            withFraction.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            let plain = ISO8601DateFormatter()
            plain.formatOptions = [.withInternetDateTime]
            let truncatedFormatter = ISO8601DateFormatter()
            truncatedFormatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            if let d = withFraction.date(from: str) { return d }
            if let d = plain.date(from: str) { return d }
            if let dotRange = str.range(of: ".") {
                let beforeFraction = String(str[..<dotRange.lowerBound])
                let afterDot = String(str[dotRange.upperBound...])
                let tzStart: String.Index
                if let zIdx = afterDot.firstIndex(of: "Z") {
                    tzStart = zIdx
                } else if let plusIdx = afterDot.range(of: #"[\+\-]\d{2}:\d{2}$"#, options: .regularExpression)?.lowerBound {
                    tzStart = plusIdx
                } else {
                    throw DecodingError.dataCorruptedError(
                        in: container, debugDescription: "无法解析时间：\(str)")
                }
                let fractionDigits = String(afterDot[..<tzStart])
                let tzPart = String(afterDot[tzStart...])
                let truncatedFraction = String(fractionDigits.prefix(3))
                let rebuilt = beforeFraction + "." + truncatedFraction + tzPart
                if let d = truncatedFormatter.date(from: rebuilt) { return d }
            }
            throw DecodingError.dataCorruptedError(
                in: container, debugDescription: "无法解析时间：\(str)")
        }
        return result
    }
}
