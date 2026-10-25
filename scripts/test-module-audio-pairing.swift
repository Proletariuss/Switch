import Foundation

@main private enum ModuleAudioPairingHarness {
    static func observation(_ id: UInt32, _ name: String, _ input: UInt32, _ output: UInt32,
                            _ related: [UInt32]?, uid: String? = "default", transport: UInt32 = 0x75736220) -> ModuleAudioDeviceObservation {
        ModuleAudioDeviceObservation(id: id, name: name, uid: uid == "default" ? "opaque:\(id)" : uid,
            transport: transport, inputStreams: input, outputStreams: output, relatedDevices: related)
    }

    static func main() {
        // Never instantiate AudioBridge.shared or call CoreAudio. These are raw
        // observations through the exact production Swift -> Rust adapter.
        let endpoints = [observation(1,"AC Interface",1,0,[1,2]), observation(2,"AS Interface",0,1,[2,1])]
        let defaults = [observation(10,"Mac speaker",0,1,[]), observation(11,"Mac mic",1,0,[])]
        func select(_ devices: [ModuleAudioDeviceObservation], speaker: UInt32 = 10, mic: UInt32 = 11) -> (input: UInt32, output: UInt32)? {
            AudioBridge.selectModuleDevices(devices, speaker: speaker, microphone: mic)
        }
        func accepted(_ devices: [ModuleAudioDeviceObservation]) {
            let pair = select(devices)
            precondition(pair?.input == 1 && pair?.output == 2, "same physical group must pair")
        }
        accepted(endpoints + defaults)
        accepted(Array((endpoints + defaults).reversed()))
        let second = [observation(3,"AS Interface",0,1,[3,4]), observation(4,"AC Interface",1,0,[4,3])]
        precondition(select([endpoints[0],second[0],endpoints[1],second[1]] + defaults) == nil)
        precondition(select(Array((second + defaults + endpoints).reversed())) == nil)
        precondition(select([observation(1,"AC Interface",1,0,[1]), endpoints[1]] + defaults) == nil)
        precondition(select([observation(1,"AC Interface",1,0,nil), endpoints[1]] + defaults) == nil)
        precondition(select([observation(1,"AC Interface",1,0,[1,2],uid:nil), endpoints[1]] + defaults) == nil)
        precondition(select([endpoints[0],observation(2,"AS Interface",0,1,[1,2],uid:"opaque:1")] + defaults) == nil)
        precondition(select([observation(1,"AC Interface",0,1,[1,2]), endpoints[1]] + defaults) == nil)
        precondition(select([endpoints[0],observation(2,"AS Interface",1,0,[1,2])] + defaults) == nil)
        precondition(select([endpoints[0],observation(2,"AS Interface",0,1,[1,2],transport:0)] + defaults) == nil)
        precondition(select(endpoints + defaults, speaker:2) == nil)
        precondition(select(endpoints + defaults, mic:1) == nil)
        let siblings = [observation(1,"AC Interface",1,0,[1,2,3]), observation(2,"AS Interface",0,1,[1,2,3]),
                        observation(3,"Other endpoint",1,1,[3,1,2])]
        accepted(siblings + defaults)
        precondition(select(siblings + defaults, speaker:3) == nil)
        precondition(select(siblings + defaults, mic:3) == nil)
        precondition(select(endpoints + [observation(10,"Mac speaker",0,1,[],uid:nil),defaults[1]]) == nil)
        precondition(select(endpoints + defaults + [defaults[0]]) == nil)
        print("PASS: module audio pairing 18 Swift/Rust scenarios (synthetic only; no CoreAudio device opened)")
    }
}
