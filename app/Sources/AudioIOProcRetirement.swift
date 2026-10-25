struct AudioIOProcRetirementFailure: Equatable {
    enum Operation: String {
        case stop = "AudioDeviceStop"
        case destroy = "AudioDeviceDestroyIOProcID"
    }

    let operation: Operation
    let status: Int32
}

/// Keeps callback resources owned until CoreAudio confirms both stop and
/// destroy. A successful stop is remembered so a destroy retry never repeats
/// an already acknowledged phase.
struct AudioIOProcRetirementLedger<Resource> {
    struct Entry {
        let resource: Resource
        var stopAcknowledged: Bool
    }

    private(set) var entries: [Entry] = []

    var isEmpty: Bool { entries.isEmpty }
    var callbacksMayRun: Bool { entries.contains { !$0.stopAcknowledged } }

    @discardableResult
    mutating func append(_ resource: Resource) -> Int {
        entries.append(Entry(resource: resource, stopAcknowledged: true))
        return entries.count - 1
    }

    mutating func markStarted(at index: Int) {
        precondition(entries.indices.contains(index))
        precondition(entries[index].stopAcknowledged)
        entries[index].stopAcknowledged = false
    }

    mutating func retire(
        stop: (Resource) -> Int32,
        destroy: (Resource) -> Int32,
        release: (Resource) -> Void
    ) -> AudioIOProcRetirementFailure? {
        var firstFailure: AudioIOProcRetirementFailure?

        for index in entries.indices.reversed() {
            let resource = entries[index].resource
            if !entries[index].stopAcknowledged {
                let status = stop(resource)
                guard status == 0 else {
                    if firstFailure == nil {
                        firstFailure = AudioIOProcRetirementFailure(operation: .stop, status: status)
                    }
                    continue
                }
                entries[index].stopAcknowledged = true
            }

            let status = destroy(resource)
            guard status == 0 else {
                if firstFailure == nil {
                    firstFailure = AudioIOProcRetirementFailure(operation: .destroy, status: status)
                }
                continue
            }

            release(resource)
            entries.remove(at: index)
        }

        return firstFailure
    }
}
