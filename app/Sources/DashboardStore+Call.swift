import Foundation

extension DashboardStore {
    // MARK: - 通话与音频状态
    var voiceError: String? { callRuntime.error ?? callRuntime.status.historyError }
    var callModeStatus: CallModeStatus? { callMode.status }
    var callModeActionInFlight: Bool { callMode.actionInFlight }
    var callModeError: String? { callMode.error }
    var callModeBackups: [CallModeUSBBackupSummary] { callMode.backups }
    var callModeBackupsLoading: Bool { callMode.backupsLoading }
    var callModeBackupError: String? { callMode.backupError }
    var callModeBackupExportingID: String? { callMode.backupExportingID }
    var callModeBackupImporting: Bool { callMode.backupImporting }
    var callModeBackupDeletingID: String? { callMode.backupDeletingID }
    var callModeRestoreInFlight: Bool { callMode.restoreInFlight }
    var callStatus: CallStatus { callRuntime.status }
    var callAnswerInFlight: Bool { callRuntime.answerInFlight }
    var callEndpoint: String {
        get { callRuntime.endpoint }
        set { callRuntime.selectEndpoint(newValue) }
    }
    var dialNumber: String {
        get { callRuntime.dialNumber }
        set { callRuntime.dialNumber = newValue }
    }
    var audioError: String? {
        get { callAudio.error }
        set { callAudio.error = newValue }
    }
    var audioRunning: Bool { callAudio.running }
    var readerAudioPhase: String { callAudio.readerPhase }
    var readerAudioInputName: String { callAudio.readerInputName }
    var readerAudioOutputName: String { callAudio.readerOutputName }
    var readerAudioProcessingName: String { callAudio.readerProcessingName }
    var readerCallState: ReaderVoWiFiCallState? { callRuntime.readerState }
    var callHistory: [CallRecord] { callHistoryStore.records }
}
