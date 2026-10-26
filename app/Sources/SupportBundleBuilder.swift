import Foundation

enum SupportBundleBuilder {
    static func make(client: ClientDiagnosticSnapshot, backend: Data?,
                     appVersion: String, build: String) throws -> Data {
        let clientData = try JSONEncoder().encode(client)
        let clientObject = try JSONSerialization.jsonObject(with: clientData)
        var warnings = client.warnings
        var runtime: Any = NSNull()
        if let backend, backend.count <= 2 * 1024 * 1024,
           let object = try JSONSerialization.jsonObject(with: backend) as? [String: Any],
           object["schema_version"] as? Int == 1 {
            // The Rust endpoint validates every persisted record against its
            // closed schema. Never bundle arbitrary legacy raw logs or extras.
            let allowed: Set<String> = ["schema_version","generated_at_ms","version","build",
                "source_digest","runtime_id","limits","counters","events","warnings"]
            runtime = object.filter { allowed.contains($0.key) }
        } else {
            warnings.append("backend_snapshot_unavailable")
        }
        let numericVersion = appVersion.utf8.allSatisfy { (48...57).contains($0) || $0 == 46 }
            && appVersion.count <= 32 ? appVersion : "unknown"
        let numericBuild = build.utf8.allSatisfy { (48...57).contains($0) }
            && build.count <= 16 ? build : "unknown"
        let package: [String: Any] = [
            "schema_version": 1, "kind": "switch-support-bundle",
            "generated_at_ms": Int64(Date().timeIntervalSince1970 * 1000),
            "app_version": numericVersion, "build": numericBuild,
            "privacy": "technical_metadata_only_no_message_bodies_credentials_or_subscriber_ids",
            "client": clientObject, "backend": runtime,
            "warnings": warnings
        ]
        let data = try JSONSerialization.data(withJSONObject: package, options: [.prettyPrinted, .sortedKeys])
        guard data.count <= 8 * 1024 * 1024 else { throw CocoaError(.fileWriteOutOfSpace) }
        return data
    }
}
