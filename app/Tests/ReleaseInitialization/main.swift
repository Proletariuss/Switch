import Foundation
import Darwin
import CryptoKit
@main @MainActor enum ReleaseInitializationTests {
 static func digest(_ data:Data)->String { SHA256.hash(data:data).map{String(format:"%02x",$0)}.joined() }
 static func verifyQueue(_ wire:Data,key:Data)->[String:Any] {
  precondition(wire.prefix(4)==Data("SWF1".utf8))
  let box=try! AES.GCM.SealedBox(nonce:AES.GCM.Nonce(data:wire[4..<16]),ciphertext:wire[16..<(wire.count-16)],tag:wire.suffix(16))
  let json=try! AES.GCM.open(box,using:SymmetricKey(data:key),authenticating:Data("SWF1".utf8))
  return try! JSONSerialization.jsonObject(with:json) as! [String:Any]
 }
 static func main() async throws {
  let mode=CommandLine.arguments[1]
  precondition(["fresh","upgrade","upgrade-reopen"].contains(mode))
  precondition(ProcessInfo.processInfo.environment["SWITCH_FORWARDING_HANDOFF_LIVE_SOCKET"]==nil)
  ForwardingKeychain.reset(); ForwardingFixture.defaults.removePersistentDomain(forName:ForwardingFixture.domain)
  defer { ForwardingFixture.defaults.removePersistentDomain(forName:ForwardingFixture.domain) }
  let queue=ForwardingFixture.checkpointDirectory.appendingPathComponent("sms-forwarding/state.enc")
  let mockSnapshot=ForwardingFixture.checkpointDirectory.appendingPathComponent("synthetic-mock-keychain.json")
  if mode=="upgrade-reopen" {
   let stored=try JSONDecoder().decode([String:Data].self,from:Data(contentsOf:mockSnapshot))
   ForwardingKeychain.reset(stored)
  }
  if mode=="upgrade" {
   let directory=queue.deletingLastPathComponent()
   try FileManager.default.createDirectory(at:directory,withIntermediateDirectories:true,attributes:[.posixPermissions:0o700])
   let hex=try String(contentsOfFile:CommandLine.arguments[2],encoding:.utf8).trimmingCharacters(in:.whitespacesAndNewlines)
   var wire=Data();var i=hex.startIndex
   while i<hex.endIndex { let end=hex.index(i,offsetBy:2);wire.append(UInt8(hex[i..<end],radix:16)!);i=end }
   try wire.write(to:queue);precondition(chmod(queue.path,0o600)==0)
   let key=Data((0..<32).map(UInt8.init))
   precondition((verifyQueue(wire,key:key)["pending"] as! [String:Any]).count==2)
   ForwardingKeychain.reset(["telegram.botToken":Data("synthetic-release-token".utf8),"telegram.proxyURL":Data("".utf8),"bark.deviceKey":Data("synthetic-release-device".utf8),"bark.encryptionKey":Data("synthetic-release-encryption".utf8),"state.encryptionKey":Data(key.base64EncodedString().utf8)])
   ForwardingFixture.defaults.set(true,forKey:"switch.smsForwarding.telegram.enabled")
   ForwardingFixture.defaults.set("synthetic-release-chat",forKey:"switch.smsForwarding.telegram.chatID")
   ForwardingFixture.defaults.set(true,forKey:"switch.smsForwarding.bark.enabled")
   let legacy=SMSForwardingStore()
   precondition(legacy.credentialLoadFailure==nil && legacy.credentialScopeLoaded)
   precondition(legacy.telegramEnabled && legacy.barkEnabled)
   let afterLegacy = try Data(contentsOf:queue); precondition(afterLegacy==wire)
   print("PASS synthetic legacy upgrade creates complete active configuration while preserving authenticated nonempty SWF1 (2 pending)")
  }
  let first=SMSForwardingStore()
  precondition(first.credentialLoadFailure==nil && first.credentialScopeLoaded && first.appliedConfiguration != nil)
  let queueBefore=try Data(contentsOf:queue), credentialsBefore=ForwardingKeychain.values
  let keyBefore=first.stateEncryptionKey, intentBefore=first.configurationIntent, activeBefore=first.loadedActiveAccount
  let plaintext=verifyQueue(queueBefore,key:Data(base64Encoded:keyBefore)!)
  if mode=="fresh" {
   precondition(!first.telegramEnabled && !first.barkEnabled)
   precondition(first.telegramBotToken.isEmpty && first.telegramChatID.isEmpty && first.telegramProxyURL.isEmpty && first.barkDeviceKey.isEmpty && first.barkEncryptionKey.isEmpty)
   precondition((plaintext["pending"] as! [String:Any]).isEmpty)
   print("PASS fresh no-state loads blank provider credentials, both providers disabled, authenticated empty queue")
  } else {
   precondition(first.telegramEnabled && first.barkEnabled)
   precondition(first.telegramBotToken=="synthetic-release-token" && first.barkDeviceKey=="synthetic-release-device" && first.barkEncryptionKey=="synthetic-release-encryption")
   precondition((plaintext["pending"] as! [String:Any]).count==2)
   print("PASS overwrite baseline includes enabled synthetic provider configuration, original key and 2 pending encrypted tasks")
  }
  ForwardingKeychain.writes=[]; ForwardingKeychain.randomCalls=0
  // Fresh process reconstruction path after replacing only application bits; same data root and mock Keychain.
  let reopened=SMSForwardingStore()
  precondition(reopened.credentialLoadFailure==nil && reopened.credentialScopeLoaded)
  precondition(reopened.stateEncryptionKey==keyBefore && reopened.configurationIntent==intentBefore && reopened.loadedActiveAccount==activeBefore)
  precondition(reopened.telegramEnabled==first.telegramEnabled && reopened.barkEnabled==first.barkEnabled && reopened.telegramBotToken==first.telegramBotToken && reopened.barkDeviceKey==first.barkDeviceKey && reopened.barkEncryptionKey==first.barkEncryptionKey)
  precondition(ForwardingKeychain.values==credentialsBefore && ForwardingKeychain.writes.isEmpty && ForwardingKeychain.randomCalls==0)
  let afterReopen = try Data(contentsOf:queue); precondition(afterReopen==queueBefore)
  precondition(ForwardingFixture.calls.isEmpty && ForwardingFixture.testBodies.isEmpty && ForwardingFixture.telegramTestCalls==0 && ForwardingFixture.liveRequests.isEmpty)
  print("PASS \(mode) reopen preserves exact mock Keychain, active pointer, config intent, key and queue SHA256 \(digest(queueBefore)); zero provider/network calls")
  if mode != "fresh" {
   let bundle=ForwardingFixture.checkpointDirectory.appendingPathComponent("synthetic-installed/Switch.app")
   try FileManager.default.createDirectory(at:bundle,withIntermediateDirectories:true)
   try Data("synthetic-old-application-bits".utf8).write(to:bundle.appendingPathComponent("program"))
   try FileManager.default.removeItem(at:bundle)
   try FileManager.default.createDirectory(at:bundle,withIntermediateDirectories:true)
   try Data("synthetic-new-application-bits".utf8).write(to:bundle.appendingPathComponent("program"))
   let reinstalled=SMSForwardingStore()
   let retainedQueue=try Data(contentsOf:queue)
   precondition(reinstalled.telegramEnabled && reinstalled.barkEnabled && reinstalled.stateEncryptionKey==keyBefore && retainedQueue==queueBefore)
   precondition(ForwardingKeychain.values==credentialsBefore && ForwardingKeychain.writes.isEmpty && ForwardingKeychain.randomCalls==0)
   precondition(ForwardingFixture.calls.isEmpty && ForwardingFixture.liveRequests.isEmpty)
   print("PASS delete-only synthetic .app plus reinstall keeps existing state; it is not a clean install and never resets credentials/queue")
   if mode=="upgrade" {
    try JSONEncoder().encode(credentialsBefore).write(to:mockSnapshot)
    precondition(chmod(mockSnapshot.path,0o600)==0)
   } else {
    let stored=try JSONDecoder().decode([String:Data].self,from:Data(contentsOf:mockSnapshot))
    precondition(stored==ForwardingKeychain.values)
    print("PASS separate process reloads exact synthetic mock Keychain + enabled configuration + authenticated 2-pending queue without mutation")
   }
  }
 }
}
