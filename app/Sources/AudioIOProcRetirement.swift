struct AudioIOProcRetirementFailure: Equatable {
    enum Operation: String {
        case stop = "AudioDeviceStop"
        case destroy = "AudioDeviceDestroyIOProcID"
    }

    let operation: Operation
    let status: Int32
}

struct AudioIOProcLifecycleSnapshot: Equatable {
    let running: Bool
    let retirementPending: Bool
}

/// A value-only CoreAudio client-data token, never an address to dereference.
/// This project freezes 64-bit macOS. Keeping tokens within the low 32 bits
/// avoids tagged/noncanonical pointer values on its arm64 target. Generation
/// zero is invalid and exhaustion fails closed instead of reusing an old token.
struct AudioIOProcCookie: Equatable {
    static let maximumGeneration = UInt(UInt32.max) >> 2
    static var supportedPlatform: Bool {
#if os(macOS) && arch(arm64)
        MemoryLayout<UnsafeMutableRawPointer>.size == 8 && MemoryLayout<UInt>.size == 8
#else
        false
#endif
    }
    let generation: UInt
    let role: Int

    init?(generation: UInt, role: Int) {
        guard Self.supportedPlatform,
              generation > 0, generation <= Self.maximumGeneration,
              (0..<4).contains(role) else { return nil }
        self.generation = generation
        self.role = role
    }

    init?(opaque: UnsafeMutableRawPointer?) {
        guard let opaque else { return nil }
        let value = UInt(bitPattern: opaque)
        guard value <= UInt(UInt32.max) else { return nil }
        self.init(generation: value >> 2, role: Int(value & 3))
    }

    var opaque: UnsafeMutableRawPointer {
        // A validated nonzero generation always produces a nonzero token.
        UnsafeMutableRawPointer(bitPattern: (generation << 2) | UInt(role))!
    }
}

/// Confined to AudioBridge's existing bridgeLock, shared with pipeline access
/// and reset. It owns no callback memory and does not infer HAL quiescence.
struct AudioIOProcGenerationFence {
    private(set) var lastGeneration: UInt
    private(set) var activeGeneration: UInt?

    init(lastGeneration: UInt = 0) {
        self.lastGeneration = lastGeneration
    }

    mutating func begin() -> UInt? {
        guard activeGeneration == nil,
              lastGeneration < AudioIOProcCookie.maximumGeneration,
              AudioIOProcCookie.supportedPlatform else { return nil }
        lastGeneration += 1
        activeGeneration = lastGeneration
        return lastGeneration
    }

    mutating func deactivate() { activeGeneration = nil }

    func admits(_ cookie: AudioIOProcCookie) -> Bool {
        activeGeneration == cookie.generation
    }
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
    // This is an acknowledgement ledger, not a callback-drain observation.
    // Pipeline reset still needs the generation fence under bridgeLock.
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
