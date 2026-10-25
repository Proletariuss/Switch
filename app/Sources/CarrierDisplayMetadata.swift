import Foundation

// MARK: - MCC → 国旗 emoji 工具

/// MCC → ISO 3166-1 alpha-2 国家码
func mccToCountryCode(_ mcc: String) -> String? {
    switch mcc {
    case "460", "461": return "CN"
    case "234", "235": return "GB"
    case "310", "311", "312", "313", "314", "315", "316": return "US"
    case "208": return "FR"
    case "262": return "DE"
    case "222": return "IT"
    case "214": return "ES"
    case "232": return "AT"
    case "228": return "CH"
    case "204": return "NL"
    case "260": return "PL"
    case "242": return "NO"
    case "244": return "FI"
    case "240": return "SE"
    case "238": return "DK"
    case "241": return "FO"
    case "248": return "EE"
    case "272": return "IE"
    case "286": return "TR"
    case "250": return "RU"
    case "505": return "AU"
    case "530": return "NZ"
    case "440", "441": return "JP"
    case "450": return "KR"
    case "454": return "HK"
    case "455": return "MO"
    case "466": return "TW"
    case "293": return "SI"
    case "294": return "MK"
    case "295": return "LI"
    case "452": return "VN"
    case "520": return "TH"
    case "525": return "SG"
    case "404", "405", "406": return "IN"
    case "724": return "BR"
    case "730": return "CL"
    case "722": return "AR"
    case "334": return "MX"
    case "732": return "CO"
    case "602": return "EG"
    case "655": return "ZA"
    case "659": return "SS"
    case "604": return "MA"
    case "612": return "CI"
    case "631": return "AO"
    case "425": return "IL"
    case "424": return "AE"
    case "427": return "QA"
    case "420": return "SA"
    case "422": return "OM"
    case "429": return "NP"
    case "470": return "BD"
    case "413": return "LK"
    case "414": return "MM"
    case "457": return "LA"
    case "456": return "KH"
    case "502": return "MY"
    case "510": return "ID"
    case "515": return "PH"
    case "620": return "GH"
    case "614": return "NE"
    case "621": return "NG"
    case "619": return "SL"
    case "629": return "CG"
    case "639": return "KE"
    case "640": return "TZ"
    case "641": return "UG"
    case "648": return "ZW"
    case "643": return "MZ"
    case "645": return "ZM"
    case "649": return "NA"
    case "652": return "BW"
    case "646": return "MG"
    case "647": return "RE"
    default: return nil
    }
}

/// ISO 3166-1 alpha-2 → 国旗 emoji
func flagEmoji(countryCode: String) -> String {
    let countryCode = countryCode.trimmingCharacters(in: .whitespacesAndNewlines).uppercased()
    guard countryCode.unicodeScalars.count == 2 else { return "" }
    let base: UInt32 = 0x1F1E6
    var result = ""
    for scalar in countryCode.unicodeScalars {
        guard scalar.isASCII, scalar.value >= 0x41 && scalar.value <= 0x5A else { return "" }
        if let flagScalar = Unicode.Scalar(base + (scalar.value - 0x41)) {
            result.unicodeScalars.append(flagScalar)
        }
    }
    return result
}

/// E.164 国家/地区呼叫码 → ISO 3166-1 alpha-2。
/// 对共享号码区采用最具体的已分配前缀；无法区分的 +1/+7 分别回退 US/RU。
private let e164CountryCodes: [String: String] = [
    "1242":"BS", "1246":"BB", "1264":"AI", "1268":"AG", "1284":"VG",
    "1340":"VI", "1345":"KY", "1441":"BM", "1473":"GD", "1649":"TC",
    "1664":"MS", "1670":"MP", "1671":"GU", "1684":"AS", "1721":"SX",
    "1758":"LC", "1767":"DM", "1784":"VC", "1787":"PR", "1809":"DO",
    "1829":"DO", "1849":"DO", "1868":"TT", "1869":"KN", "1876":"JM",
    "211":"SS", "212":"MA", "213":"DZ", "216":"TN", "218":"LY",
    "220":"GM", "221":"SN", "222":"MR", "223":"ML", "224":"GN", "225":"CI",
    "226":"BF", "227":"NE", "228":"TG", "229":"BJ", "230":"MU", "231":"LR",
    "232":"SL", "233":"GH", "234":"NG", "235":"TD", "236":"CF", "237":"CM",
    "238":"CV", "239":"ST", "240":"GQ", "241":"GA", "242":"CG", "243":"CD",
    "244":"AO", "245":"GW", "246":"IO", "248":"SC", "249":"SD", "250":"RW",
    "251":"ET", "252":"SO", "253":"DJ", "254":"KE", "255":"TZ", "256":"UG",
    "257":"BI", "258":"MZ", "260":"ZM", "261":"MG", "262":"RE", "263":"ZW",
    "264":"NA", "265":"MW", "266":"LS", "267":"BW", "268":"SZ", "269":"KM",
    "290":"SH", "291":"ER", "297":"AW", "298":"FO", "299":"GL",
    "350":"GI", "351":"PT", "352":"LU", "353":"IE", "354":"IS", "355":"AL",
    "356":"MT", "357":"CY", "358":"FI", "359":"BG", "370":"LT", "371":"LV",
    "372":"EE", "373":"MD", "374":"AM", "375":"BY", "376":"AD", "377":"MC",
    "378":"SM", "379":"VA", "380":"UA", "381":"RS", "382":"ME", "383":"XK",
    "385":"HR", "386":"SI", "387":"BA", "389":"MK", "420":"CZ", "421":"SK",
    "423":"LI", "500":"FK", "501":"BZ", "502":"GT", "503":"SV", "504":"HN",
    "505":"NI", "506":"CR", "507":"PA", "508":"PM", "509":"HT", "590":"GP",
    "591":"BO", "592":"GY", "593":"EC", "594":"GF", "595":"PY", "596":"MQ",
    "597":"SR", "598":"UY", "599":"CW", "670":"TL", "672":"NF", "673":"BN",
    "674":"NR", "675":"PG", "676":"TO", "677":"SB", "678":"VU", "679":"FJ",
    "680":"PW", "681":"WF", "682":"CK", "683":"NU", "685":"WS", "686":"KI",
    "687":"NC", "688":"TV", "689":"PF", "690":"TK", "691":"FM", "692":"MH",
    "850":"KP", "852":"HK", "853":"MO", "855":"KH", "856":"LA", "880":"BD",
    "886":"TW", "960":"MV", "961":"LB", "962":"JO", "963":"SY", "964":"IQ",
    "965":"KW", "966":"SA", "967":"YE", "968":"OM", "970":"PS", "971":"AE",
    "972":"IL", "973":"BH", "974":"QA", "975":"BT", "976":"MN", "977":"NP",
    "992":"TJ", "993":"TM", "994":"AZ", "995":"GE", "996":"KG", "998":"UZ",
    "20":"EG", "27":"ZA", "30":"GR", "31":"NL", "32":"BE", "33":"FR",
    "34":"ES", "36":"HU", "39":"IT", "40":"RO", "41":"CH", "43":"AT",
    "44":"GB", "45":"DK", "46":"SE", "47":"NO", "48":"PL", "49":"DE",
    "51":"PE", "52":"MX", "53":"CU", "54":"AR", "55":"BR", "56":"CL",
    "57":"CO", "58":"VE", "60":"MY", "61":"AU", "62":"ID", "63":"PH",
    "64":"NZ", "65":"SG", "66":"TH", "76":"KZ", "77":"KZ", "81":"JP",
    "82":"KR", "84":"VN", "86":"CN", "90":"TR", "91":"IN", "92":"PK",
    "93":"AF", "94":"LK", "95":"MM", "98":"IR", "1":"US", "7":"RU"
]

/// ICCID 前缀 → ISO 3166-1 alpha-2 国家码。
/// ICCID 结构：89 + E.164 国家/地区码 + 发行方标识 + 账户标识。
func iccidToCountryCode(_ iccid: String) -> String? {
    let digits = iccid.filter(\.isNumber)
    guard digits.hasPrefix("89"), digits.count >= 4 else { return nil }
    let issuer = String(digits.dropFirst(2))
    for length in stride(from: min(4, issuer.count), through: 1, by: -1) {
        let prefix = String(issuer.prefix(length))
        if let country = e164CountryCodes[prefix] { return country }
    }
    return nil
}

/// MCC → 国旗 emoji
func mccFlagEmoji(_ mcc: String) -> String? {
    guard let cc = mccToCountryCode(mcc) else { return nil }
    let emoji = flagEmoji(countryCode: cc)
    return emoji.isEmpty ? nil : emoji
}

/// MCC → localized country/region name. Keep the MCC table as the single
/// source of truth so flags and labels cannot drift apart between pages.
func mccCountryDisplayName(
    _ mcc: String,
    locale: Locale = Locale(identifier: "zh_Hans")
) -> String? {
    guard let countryCode = mccToCountryCode(mcc) else { return nil }
    return locale.localizedString(forRegionCode: countryCode)
}

/// ICCID → 国旗 emoji
func iccidFlagEmoji(_ iccid: String) -> String? {
    guard let cc = iccidToCountryCode(iccid) else { return nil }
    let emoji = flagEmoji(countryCode: cc)
    return emoji.isEmpty ? nil : emoji
}

/// 从已登记的 E.164 号码推断国家旗帜，作为 ICCID 国家前缀未知时的回退。
func phoneFlagEmoji(_ phoneNumber: String) -> String? {
    let trimmed = phoneNumber.trimmingCharacters(in: .whitespacesAndNewlines)
    guard trimmed.hasPrefix("+") else { return nil }
    let digits = trimmed.dropFirst().filter(\.isNumber)
    guard !digits.isEmpty else { return nil }
    for length in stride(from: min(4, digits.count), through: 1, by: -1) {
        let prefix = String(digits.prefix(length))
        if let countryCode = e164CountryCodes[prefix] {
            let emoji = flagEmoji(countryCode: countryCode)
            return emoji.isEmpty ? nil : emoji
        }
    }
    return nil
}

/// 清理 Profile 名称：去除日期后缀和前导 "+"
func cleanProfileDisplayName(_ name: String?) -> String {
    guard let name = name?.trimmingCharacters(in: .whitespacesAndNewlines), !name.isEmpty else {
        return "SIM"
    }
    var result = name
    if let dashRange = result.range(of: #"-\d{4}\.\d{1,2}\.\d{1,2}$"#, options: .regularExpression) {
        result = String(result[..<dashRange.lowerBound])
    }
    if result.hasPrefix("+") {
        result = String(result.dropFirst())
    }
    return result
}
