#!/usr/bin/env python3
"""Check acceptance bookkeeping without awarding functional acceptance.

This reads source files only. It does not open devices, start a backend, run a
compiler, infer old support from current hardware, or promote historical tests.
Use --require-frozen only when a frozen denominator is the intended gate.  A
product-final release must use --require-product-complete: freezing a
denominator is bookkeeping and never implies functional/product acceptance.
"""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
BASE = Path("docs/optimization/all-rust")
FILES = {
    "ledger": "acceptance-ledger-2026-10-19.json",
    "mapping": "entry-acceptance-map.json",
    "protocol": "acceptance-scope-protocol.json",
    "baselines": "baseline-provenance.json",
    "inventory": "api-contract-inventory.json",
}
RUNTIME_MAP = "docs/optimization/all-rust/runtime-owner-format-exit-map.json"
# The active trace advances explicitly; its predecessor remains immutable evidence.
P0_QMI_CURRENT = {'historical_iteration': 691,
 'historical_review': 'docs/optimization/all-rust/iteration-691-p0-qmi-owner-trace.json',
 'historical_source_snapshot': 'docs/optimization/all-rust/iteration-691-source-snapshot.json',
 'iteration': 764,
 'path': 'docs/optimization/all-rust/iteration-764-p0-qmi-owner-trace.json',
 'source_snapshot': 'docs/optimization/all-rust/iteration-764-source-snapshot.json'}
P0_QMI_REVIEW = P0_QMI_CURRENT["path"]
CURRENT_SOURCE_SNAPSHOT = {'added_files': {'docs/optimization/all-rust/iteration-764-ledger-source-conservation.json',
                 'docs/optimization/all-rust/iteration-764-source-refresh-review.md',
                 'rust/crates/proletariusd/src/forwarding_admission_store/raw_tests.rs',
                 'rust/crates/proletariusd/src/http_server.rs',
                 'rust/crates/proletariusd/src/ims_sms_transactions/nwait_warning_tests.rs',
                 'rust/crates/proletariusd/src/multistandby_native_runtime/start_admission_tests.rs',
                 'rust/crates/proletariusd/src/native_vowifi_session/ordinary/status/tests.rs',
                 'rust/crates/proletariusd/src/native_vowifi_session/service/production_integration/facade_exchange.rs',
                 'rust/crates/proletariusd/src/native_vowifi_session/service/production_integration/manager_multi_profile.rs',
                 'rust/crates/proletariusd/src/native_vowifi_session/service/production_integration/manager_multi_profile/warning_756.rs',
                 'rust/crates/proletariusd/src/network_recovery/coordinator_owner.rs',
                 'rust/crates/proletariusd/src/network_recovery/coordinator_owner_tests.rs',
                 'rust/crates/proletariusd/src/profile_notes_read_offload_tests.rs',
                 'rust/crates/proletariusd/src/sms_store_health/raw_tests.rs'},
 'changed_files': {'app/Sources/BluetoothReaderManager.swift',
                   'app/Sources/CallAudioStore.swift',
                   'app/Sources/ModuleNetworkStore.swift',
                   'app/Sources/ModuleVoWiFiStore.swift',
                   'app/Sources/MultiStandbyStore.swift',
                   'app/Sources/ProletariusApp.swift',
                   'app/Sources/ReaderRuntimeStore.swift',
                   'app/Sources/ReaderVoWiFiAudioBridge.swift',
                   'app/Sources/SMSForwardingStore.swift',
                   'app/Sources/UnixSocketURLProtocol.swift',
                   'app/Sources/Views/HomeView.swift',
                   'app/Sources/Views/SMSForwardingSettingsView.swift',
                   'app/Tests/DeviceCallCorrectness/main.swift',
                   'app/Tests/ModuleNetworkRecovery/Adapters.swift',
                   'app/Tests/ModuleNetworkRecovery/main.swift',
                   'app/Tests/MultiStandby/main.swift',
                   'app/Tests/SMSForwardingClient/main.swift',
                   'app/Tests/SMSForwardingClient/prepare.rb',
                   'app/Tests/VoWiFiReadiness/Adapters.swift',
                   'app/Tests/VoWiFiReadiness/OperationTests.swift',
                   'app/Tests/VoWiFiReadiness/main.swift',
                   'docs/optimization/all-rust/api-contract-inventory.json',
                   'docs/optimization/all-rust/api-contract-review.json',
                   'rust/Cargo.lock',
                   'rust/crates/proletarius-forwarding/src/lib.rs',
                   'rust/crates/proletarius-forwarding/src/provider.rs',
                   'rust/crates/proletarius-forwarding/src/queue_store.rs',
                   'rust/crates/proletarius-forwarding/src/worker.rs',
                   'rust/crates/proletarius-store/src/lib.rs',
                   'rust/crates/proletarius-vowifi/src/ims_profile.rs',
                   'rust/crates/proletarius-vowifi/src/registration_coordinator.rs',
                   'rust/crates/proletarius-vowifi/src/sip_session_runtime.rs',
                   'rust/crates/proletariusd/src/archive_io.rs',
                   'rust/crates/proletariusd/src/call_archive.rs',
                   'rust/crates/proletariusd/src/call_audio.rs',
                   'rust/crates/proletariusd/src/call_codec.rs',
                   'rust/crates/proletariusd/src/call_control.rs',
                   'rust/crates/proletariusd/src/call_facts.rs',
                   'rust/crates/proletariusd/src/call_mode_adb.rs',
                   'rust/crates/proletariusd/src/call_mode_backups.rs',
                   'rust/crates/proletariusd/src/call_mode_enable.rs',
                   'rust/crates/proletariusd/src/call_mode_lifecycle.rs',
                   'rust/crates/proletariusd/src/call_mode_lifecycle/tests.rs',
                   'rust/crates/proletariusd/src/call_mode_restore.rs',
                   'rust/crates/proletariusd/src/call_mode_retry.rs',
                   'rust/crates/proletariusd/src/call_mode_runtime_download.rs',
                   'rust/crates/proletariusd/src/call_mode_status.rs',
                   'rust/crates/proletariusd/src/carrier.rs',
                   'rust/crates/proletariusd/src/device_send_store.rs',
                   'rust/crates/proletariusd/src/device_sms_send.rs',
                   'rust/crates/proletariusd/src/eap_identity.rs',
                   'rust/crates/proletariusd/src/esim_mutation_intent.rs',
                   'rust/crates/proletariusd/src/forwarding_admission_store.rs',
                   'rust/crates/proletariusd/src/forwarding_handoff_store.rs',
                   'rust/crates/proletariusd/src/ims_codec.rs',
                   'rust/crates/proletariusd/src/ims_sms_transactions.rs',
                   'rust/crates/proletariusd/src/main.rs',
                   'rust/crates/proletariusd/src/message_archive.rs',
                   'rust/crates/proletariusd/src/modem_actor.rs',
                   'rust/crates/proletariusd/src/module_call.rs',
                   'rust/crates/proletariusd/src/module_esim.rs',
                   'rust/crates/proletariusd/src/module_radio.rs',
                   'rust/crates/proletariusd/src/module_radio/tests.rs',
                   'rust/crates/proletariusd/src/multistandby_native_inventory.rs',
                   'rust/crates/proletariusd/src/multistandby_native_runtime.rs',
                   'rust/crates/proletariusd/src/multistandby_native_runtime/projection.rs',
                   'rust/crates/proletariusd/src/multistandby_operations.rs',
                   'rust/crates/proletariusd/src/multistandby_send.rs',
                   'rust/crates/proletariusd/src/multistandby_store.rs',
                   'rust/crates/proletariusd/src/native_ims_calls.rs',
                   'rust/crates/proletariusd/src/native_sms_consumer.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/api.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/api/tests.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/intent.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/module.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/module/request.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/multi.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/multi/startup.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/operation.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/operation/tests.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/ordinary.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/ordinary/status.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/registration_candidates.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/service.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/service/production_integration.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/service/startup_http_tests.rs',
                   'rust/crates/proletariusd/src/native_vowifi_session/service/tests.rs',
                   'rust/crates/proletariusd/src/network_recovery.rs',
                   'rust/crates/proletariusd/src/network_recovery/tests.rs',
                   'rust/crates/proletariusd/src/network_services.rs',
                   'rust/crates/proletariusd/src/ordinary_vowifi_admission.rs',
                   'rust/crates/proletariusd/src/ownership.rs',
                   'rust/crates/proletariusd/src/profile_notes.rs',
                   'rust/crates/proletariusd/src/proxy_policy.rs',
                   'rust/crates/proletariusd/src/proxy_probe.rs',
                   'rust/crates/proletariusd/src/public_http.rs',
                   'rust/crates/proletariusd/src/public_http/tests.rs',
                   'rust/crates/proletariusd/src/public_mutation_receipt.rs',
                   'rust/crates/proletariusd/src/qmi_enhancement.rs',
                   'rust/crates/proletariusd/src/reader_auth.rs',
                   'rust/crates/proletariusd/src/reader_ble_control.rs',
                   'rust/crates/proletariusd/src/reader_control.rs',
                   'rust/crates/proletariusd/src/reader_device_identity.rs',
                   'rust/crates/proletariusd/src/reader_eap_aka.rs',
                   'rust/crates/proletariusd/src/reader_esim_download.rs',
                   'rust/crates/proletariusd/src/reader_identity.rs',
                   'rust/crates/proletariusd/src/reader_proxy_check.rs',
                   'rust/crates/proletariusd/src/reader_reservation/business.rs',
                   'rust/crates/proletariusd/src/reader_reservation/lifecycle.rs',
                   'rust/crates/proletariusd/src/reader_reservation/native.rs',
                   'rust/crates/proletariusd/src/request_body_budget.rs',
                   'rust/crates/proletariusd/src/safe_events.rs',
                   'rust/crates/proletariusd/src/sms_adopt.rs',
                   'rust/crates/proletariusd/src/sms_clear.rs',
                   'rust/crates/proletariusd/src/sms_codec.rs',
                   'rust/crates/proletariusd/src/sms_delete.rs',
                   'rust/crates/proletariusd/src/sms_delete_sender.rs',
                   'rust/crates/proletariusd/src/sms_device_service.rs',
                   'rust/crates/proletariusd/src/sms_export.rs',
                   'rust/crates/proletariusd/src/sms_forwarding.rs',
                   'rust/crates/proletariusd/src/sms_forwarding/retirement_tests.rs',
                   'rust/crates/proletariusd/src/sms_store_health.rs',
                   'rust/crates/proletariusd/src/transport_selection.rs',
                   'rust/crates/proletariusd/src/unified_call_control.rs',
                   'rust/crates/proletariusd/src/usb_at.rs',
                   'rust/crates/proletariusd/src/usb_at/esim_snapshot/tests.rs',
                   'rust/crates/proletariusd/src/usb_sms_receive.rs',
                   'rust/crates/proletariusd/src/usb_sms_receive_tests.rs',
                   'rust/crates/proletariusd/src/usb_sms_storage_service.rs',
                   'rust/crates/proletariusd/src/voice_enabled.rs',
                   'rust/crates/proletariusd/src/vowifi_session_policy.rs',
                   'scripts/check-acceptance-ledger.py',
                   'scripts/check-recovery-process-gates.py',
                   'scripts/check-rust-migration-contracts.py',
                   'scripts/clean-generated.rb',
                   'scripts/preflight-source.sh',
                   'scripts/test-clean-generated.rb',
                   'scripts/test-migration-gates.sh',
                   'scripts/test-reader-audio-recovery.swift',
                   'scripts/tests/test_acceptance_ledger.py',
                   'scripts/tests/test_recovery_process_gates.py'},
 'file_count': 563,
 'immutable_ancestry': [{'iteration': 691,
                         'path': 'docs/optimization/all-rust/iteration-691-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-690-source-snapshot.json',
                                               'sha256': 'bd40f69a823eabef6d5a9f1645dbe5430a335ccba3e094c4f91add5b6f1a5151'},
                         'sha256': '75fec8156450f769950f2cfe07407f813f5f127fdd23a40d9e26ff91f15e95d3'},
                        {'iteration': 690,
                         'path': 'docs/optimization/all-rust/iteration-690-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-689-source-snapshot.json',
                                               'sha256': '43f3e1375ed97f3755580694882473c188b3d72984756b1db7ca6c3d07a06047'},
                         'sha256': 'bd40f69a823eabef6d5a9f1645dbe5430a335ccba3e094c4f91add5b6f1a5151'},
                        {'iteration': 689,
                         'path': 'docs/optimization/all-rust/iteration-689-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-688-source-snapshot.json',
                                               'sha256': '561f84ed8c4818fe1c6d53e610c9cc0edd19bece307f09eeaad0e4bbaeed465f'},
                         'sha256': '43f3e1375ed97f3755580694882473c188b3d72984756b1db7ca6c3d07a06047'},
                        {'iteration': 688,
                         'path': 'docs/optimization/all-rust/iteration-688-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-687-source-snapshot.json',
                                               'sha256': 'c5bd0ccb93cfb5cb4b7dd1679912407bfd57da7ef0cf213caa2a60c17f77fa5f'},
                         'sha256': '561f84ed8c4818fe1c6d53e610c9cc0edd19bece307f09eeaad0e4bbaeed465f'},
                        {'iteration': 687,
                         'path': 'docs/optimization/all-rust/iteration-687-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-686-source-snapshot.json',
                                               'sha256': 'd3ce0dfb42979b5289375c6600698e0d2c819689894d4abd5f388d26378c9cf9'},
                         'sha256': 'c5bd0ccb93cfb5cb4b7dd1679912407bfd57da7ef0cf213caa2a60c17f77fa5f'},
                        {'iteration': 686,
                         'path': 'docs/optimization/all-rust/iteration-686-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-685-source-snapshot.json',
                                               'sha256': '7f0033cdfff82ac2ddea8d30fc142a1d4aa98ac876cc72df25dfedfa9cafdeab'},
                         'sha256': 'd3ce0dfb42979b5289375c6600698e0d2c819689894d4abd5f388d26378c9cf9'},
                        {'iteration': 685,
                         'path': 'docs/optimization/all-rust/iteration-685-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-684-source-snapshot.json',
                                               'sha256': '5eb54371aa7420dc86bd950f54e9084749038f5d526e62eb09c838c3b1051646'},
                         'sha256': '7f0033cdfff82ac2ddea8d30fc142a1d4aa98ac876cc72df25dfedfa9cafdeab'},
                        {'iteration': 684,
                         'path': 'docs/optimization/all-rust/iteration-684-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-683-source-snapshot.json',
                                               'sha256': '0fb318da0e233e8776d2d5c3f1d4911b7969939b6e7f11428ca8cfd58906f324'},
                         'sha256': '5eb54371aa7420dc86bd950f54e9084749038f5d526e62eb09c838c3b1051646'},
                        {'iteration': 683,
                         'path': 'docs/optimization/all-rust/iteration-683-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-682-source-snapshot.json',
                                               'sha256': 'fdd885081a8f73c6f68114e54d9556a02d6a0483d42f0d4ad5b08a1a2146e019'},
                         'sha256': '0fb318da0e233e8776d2d5c3f1d4911b7969939b6e7f11428ca8cfd58906f324'},
                        {'iteration': 682,
                         'path': 'docs/optimization/all-rust/iteration-682-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-681-source-snapshot.json',
                                               'sha256': '07405e5350b40870eb3763b0b5cc31cc15c00a0d63aebc5ea267a1cd0801c774'},
                         'sha256': 'fdd885081a8f73c6f68114e54d9556a02d6a0483d42f0d4ad5b08a1a2146e019'},
                        {'iteration': 681,
                         'path': 'docs/optimization/all-rust/iteration-681-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-680-source-snapshot.json',
                                               'sha256': '7c05c88d1aef603ace7a832a5a8ae7b141bef98ef893f8f85f8191c26ee6cb95'},
                         'sha256': '07405e5350b40870eb3763b0b5cc31cc15c00a0d63aebc5ea267a1cd0801c774'},
                        {'iteration': 680,
                         'path': 'docs/optimization/all-rust/iteration-680-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-677-source-snapshot.json',
                                               'sha256': 'b9506f9b70b4090e09aceed9974c73430a9d6d86c3e038005f40dc4cc0ec46b3'},
                         'sha256': '7c05c88d1aef603ace7a832a5a8ae7b141bef98ef893f8f85f8191c26ee6cb95'},
                        {'iteration': 677,
                         'path': 'docs/optimization/all-rust/iteration-677-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-676-source-snapshot.json',
                                               'sha256': '8f9108ee440801735ffa0366eb30f391ec6182a1619ac1d66a242266e7eaf0b2'},
                         'sha256': 'b9506f9b70b4090e09aceed9974c73430a9d6d86c3e038005f40dc4cc0ec46b3'},
                        {'iteration': 676,
                         'path': 'docs/optimization/all-rust/iteration-676-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-675-source-snapshot.json',
                                               'sha256': '4a3895694ad264a4ad95f3a91817afcb637a60a91b04538ad796b6347b7d2ffe'},
                         'sha256': '8f9108ee440801735ffa0366eb30f391ec6182a1619ac1d66a242266e7eaf0b2'},
                        {'iteration': 675,
                         'path': 'docs/optimization/all-rust/iteration-675-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-672-source-snapshot.json',
                                               'sha256': '4d0e2144180823dfe23948f807dba94156162d32d7ddbd77a13e6ad76f2461bc'},
                         'sha256': '4a3895694ad264a4ad95f3a91817afcb637a60a91b04538ad796b6347b7d2ffe'},
                        {'iteration': 672,
                         'path': 'docs/optimization/all-rust/iteration-672-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-671-source-snapshot.json',
                                               'sha256': 'a83de8149abbc31d16d321d6eb8772b4d4aca4379465991e41c6631adf880eb1'},
                         'sha256': '4d0e2144180823dfe23948f807dba94156162d32d7ddbd77a13e6ad76f2461bc'},
                        {'iteration': 671,
                         'path': 'docs/optimization/all-rust/iteration-671-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-670-source-snapshot.json',
                                               'sha256': '5afefe6063e948e2d6573d8e5141855dabc908db2140e043c97fd4fdf855df35'},
                         'sha256': 'a83de8149abbc31d16d321d6eb8772b4d4aca4379465991e41c6631adf880eb1'},
                        {'iteration': 670,
                         'path': 'docs/optimization/all-rust/iteration-670-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-669-source-snapshot.json',
                                               'sha256': '6d4ff106abf66886e9c49b3877b69f99f02d4cfaa3d612b3c699aaeae7a3c42d'},
                         'sha256': '5afefe6063e948e2d6573d8e5141855dabc908db2140e043c97fd4fdf855df35'},
                        {'iteration': 669,
                         'path': 'docs/optimization/all-rust/iteration-669-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-668-source-snapshot.json',
                                               'sha256': 'd8729b3084fdee7495344712dd261b65241a6db75b97b77507428c38a1c6a113'},
                         'sha256': '6d4ff106abf66886e9c49b3877b69f99f02d4cfaa3d612b3c699aaeae7a3c42d'},
                        {'iteration': 668,
                         'path': 'docs/optimization/all-rust/iteration-668-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-666-source-snapshot.json',
                                               'sha256': '46247c468f7c2a7174a3f42d752c184d7e8b12ca3f4456cc0acaa01cdb7a281d'},
                         'sha256': 'd8729b3084fdee7495344712dd261b65241a6db75b97b77507428c38a1c6a113'},
                        {'iteration': 666,
                         'path': 'docs/optimization/all-rust/iteration-666-source-snapshot.json',
                         'previous_snapshot': {'path': 'docs/optimization/all-rust/iteration-665-source-snapshot.json',
                                               'sha256': 'a60ad2f488991159038ed51f8b86b71a0541afd14d07525bcd1a4a6c6d09e5be'},
                         'sha256': '46247c468f7c2a7174a3f42d752c184d7e8b12ca3f4456cc0acaa01cdb7a281d'}],
 'immutable_documents': {'docs/optimization/all-rust/acceptance-ledger-2026-10-19-iteration-680-pre-activation.json': '33f41e2cf9c5a59b47c4e0fe6b4c3c62f0f6fe0874fb5080e30beb6ecbdb4b07',
                         'docs/optimization/all-rust/acceptance-ledger-2026-10-19-iteration-763-pre-successor.json': '4381704d7950fc2007e7ea9446dfce838f95e98f8c031fc1fbc213498ae2ab29',
                         'docs/optimization/all-rust/entry-acceptance-map-iteration-763-pre-successor.json': 'f828cd442477d42a929feed5ed2b6a89bbb8571340511828699e177a179d8338',
                         'docs/optimization/all-rust/iteration-666-no-device-software-acceptance-scope.md': 'c6bac5da2962ca40961bcae475a160bf18e85141e52f671088f7c07d592e15ca',
                         'docs/optimization/all-rust/iteration-666-p0-qmi-owner-trace.json': 'cad2aed92e62075ddc551dd284bab0545c24df74248ec9f7ef297c3bb86ddfad',
                         'docs/optimization/all-rust/iteration-666-source-refresh-review.md': '239f7a55ed4c63da9a01593740741c5ea0fc2d1f57d538d7795e18d653ee9d7d',
                         'docs/optimization/all-rust/iteration-666-source-snapshot.json': '46247c468f7c2a7174a3f42d752c184d7e8b12ca3f4456cc0acaa01cdb7a281d',
                         'docs/optimization/all-rust/iteration-666-swift-rust-boundary-decision.md': 'eae6f5fed7c9dc2c6e10ee80b3a6adb697fe7032ccdff165cf081f0f19b8ed7b',
                         'docs/optimization/all-rust/iteration-668-p0-qmi-owner-trace.json': '5b50d0467055dca0ff7fd0201f6d7a1b3111370e865f9b28a9e69e377c17f787',
                         'docs/optimization/all-rust/iteration-668-source-refresh-review.md': '4b5620423b8a2f0822e9362b46cc04f89c57f645e129f32e5934f0aec47e9b50',
                         'docs/optimization/all-rust/iteration-668-source-snapshot.json': 'd8729b3084fdee7495344712dd261b65241a6db75b97b77507428c38a1c6a113',
                         'docs/optimization/all-rust/iteration-669-p0-qmi-owner-trace.json': 'e19078d9279a4150485ec4c2c201d91b5ce1c7a353ca96f2ec6578f83d665863',
                         'docs/optimization/all-rust/iteration-669-source-refresh-review.md': '512a0b60043752009998ed8bbeeb6d7a9874be370ca6dbce72cb2e7070810161',
                         'docs/optimization/all-rust/iteration-669-source-snapshot.json': '6d4ff106abf66886e9c49b3877b69f99f02d4cfaa3d612b3c699aaeae7a3c42d',
                         'docs/optimization/all-rust/iteration-670-p0-qmi-owner-trace.json': 'ead6ab61054d5045ad6004f5904c78b868c1e07ec8f27eb5bd7eaef6f29654f0',
                         'docs/optimization/all-rust/iteration-670-source-refresh-review.md': 'bbadcffdbf088d3b26f20ee808a41489eceab2f55831a00a4dd5cb6fa0573bb5',
                         'docs/optimization/all-rust/iteration-670-source-snapshot.json': '5afefe6063e948e2d6573d8e5141855dabc908db2140e043c97fd4fdf855df35',
                         'docs/optimization/all-rust/iteration-671-p0-qmi-owner-trace.json': '12a78f2668ae4d88c237bada725850679fdb35cb4b255afefd29ef64ad4015c5',
                         'docs/optimization/all-rust/iteration-671-source-refresh-review.md': '6d0e0cec87ba4b8f8360a95f0856f29b49fd25ea39b7b54cc4c609e6aaeaab3e',
                         'docs/optimization/all-rust/iteration-671-source-snapshot.json': 'a83de8149abbc31d16d321d6eb8772b4d4aca4379465991e41c6631adf880eb1',
                         'docs/optimization/all-rust/iteration-672-p0-qmi-owner-trace.json': 'becce3b3580c516755ebd00807e129d86fbe8dbc009ee29f550dd4b37b6ef06d',
                         'docs/optimization/all-rust/iteration-672-source-refresh-review.md': '03aa570c5addfab1eaf0fd4a887fabb419c55e8508d203f25fff3dfc13ff59a5',
                         'docs/optimization/all-rust/iteration-672-source-snapshot.json': '4d0e2144180823dfe23948f807dba94156162d32d7ddbd77a13e6ad76f2461bc',
                         'docs/optimization/all-rust/iteration-675-p0-qmi-owner-trace.json': '9042699942179170cae32404e3c2aa9bb3f70fc48b1f2b64e3a1cb80e15c774b',
                         'docs/optimization/all-rust/iteration-675-source-refresh-review.md': '5f8c1dec29556e08cbb2a13de04f868a2c660f418c20313d877ca2331e651d40',
                         'docs/optimization/all-rust/iteration-675-source-snapshot.json': '4a3895694ad264a4ad95f3a91817afcb637a60a91b04538ad796b6347b7d2ffe',
                         'docs/optimization/all-rust/iteration-676-p0-qmi-owner-trace.json': 'b746c09aaba0a5ccfd3287dc4eb97bae348a8b40e0e8e84f1b339fde9df74870',
                         'docs/optimization/all-rust/iteration-676-source-refresh-review.md': 'e8d4ab02aba3c427289162217322ec7a734fada2ce503294fb5a81defd629890',
                         'docs/optimization/all-rust/iteration-676-source-snapshot.json': '8f9108ee440801735ffa0366eb30f391ec6182a1619ac1d66a242266e7eaf0b2',
                         'docs/optimization/all-rust/iteration-677-p0-qmi-owner-trace.json': 'c1ab3f10fb9e88cacbb21369a0b00f1b93329393ad4df73e0f168301d1570609',
                         'docs/optimization/all-rust/iteration-677-source-refresh-review.md': 'fd18e477160ec615a1cf8db29d928aebf5bedb0984cd842d9b306f44dbd382b5',
                         'docs/optimization/all-rust/iteration-677-source-snapshot.json': 'b9506f9b70b4090e09aceed9974c73430a9d6d86c3e038005f40dc4cc0ec46b3',
                         'docs/optimization/all-rust/iteration-680-p0-qmi-owner-trace.json': 'bb2623607d24e8fd3822a0a9414ba17a8c65707b42c33e90989eb3c1e5327ca6',
                         'docs/optimization/all-rust/iteration-680-source-refresh-review.md': '4ad8693599b9ca62596eb3e809f39dfe1067f719e22d47f8095fbc3c36c433a2',
                         'docs/optimization/all-rust/iteration-680-source-snapshot.json': '7c05c88d1aef603ace7a832a5a8ae7b141bef98ef893f8f85f8191c26ee6cb95',
                         'docs/optimization/all-rust/iteration-681-p0-qmi-owner-trace.json': 'eb4c65adf24f8ed879309ccee33c3b9d3c7b14d215f90a000db77afced3c3465',
                         'docs/optimization/all-rust/iteration-681-source-refresh-review.md': 'c8c09df04dd9aa6171bbd396d756481773c3e912250b81ae900fa80af36a0b87',
                         'docs/optimization/all-rust/iteration-681-source-snapshot.json': '07405e5350b40870eb3763b0b5cc31cc15c00a0d63aebc5ea267a1cd0801c774',
                         'docs/optimization/all-rust/iteration-682-p0-qmi-owner-trace.json': '7da00edf9aaa6572c0574b806cebc34700325c0caef49506a5a34de0849fdfc4',
                         'docs/optimization/all-rust/iteration-682-source-refresh-review.md': '0d8fa2fea1ebe1ae3ca9b0ed3bd4aab9ef2debd6fce16f4614dda7ce2e2addc6',
                         'docs/optimization/all-rust/iteration-682-source-snapshot.json': 'fdd885081a8f73c6f68114e54d9556a02d6a0483d42f0d4ad5b08a1a2146e019',
                         'docs/optimization/all-rust/iteration-683-p0-qmi-owner-trace.json': 'a7240bf98d2f7a6ad1cfde40065890f491fba772cb3127757f7cc9e12a1cb8bd',
                         'docs/optimization/all-rust/iteration-683-source-refresh-review.md': 'c9c2d064bcbb9a61b4ae99d25638e7c93faede85558d0d277e4b105c115243d2',
                         'docs/optimization/all-rust/iteration-683-source-snapshot.json': '0fb318da0e233e8776d2d5c3f1d4911b7969939b6e7f11428ca8cfd58906f324',
                         'docs/optimization/all-rust/iteration-684-p0-qmi-owner-trace.json': '7484596ab0f0412af6b85147f21df2201c1a8826311fa148a309f6e5a61e73e1',
                         'docs/optimization/all-rust/iteration-684-source-refresh-review.md': '072dfe632d581fa6f7351eeeb9f6b91811e0ac917d682aadd3950c42958f7800',
                         'docs/optimization/all-rust/iteration-684-source-snapshot.json': '5eb54371aa7420dc86bd950f54e9084749038f5d526e62eb09c838c3b1051646',
                         'docs/optimization/all-rust/iteration-685-p0-qmi-owner-trace.json': '7fd06507d292c8190a9b5bda5dcb2e6fa62830809f116aebecb6088e7188bf22',
                         'docs/optimization/all-rust/iteration-685-source-refresh-review.md': '9630a90a48def08fbac0c1831094cab85c2f09ab80a81e4913721973788689c8',
                         'docs/optimization/all-rust/iteration-685-source-snapshot.json': '7f0033cdfff82ac2ddea8d30fc142a1d4aa98ac876cc72df25dfedfa9cafdeab',
                         'docs/optimization/all-rust/iteration-686-p0-qmi-owner-trace.json': 'a2864434f908a112d475fd255efc50580dd34dd5eb0b0827c802f37b3b7bc817',
                         'docs/optimization/all-rust/iteration-686-source-refresh-review.md': 'ab1cb5bd1dd07c33f65b928c00313396ca09bf8c86b13eefc42059fc60dd9347',
                         'docs/optimization/all-rust/iteration-686-source-snapshot.json': 'd3ce0dfb42979b5289375c6600698e0d2c819689894d4abd5f388d26378c9cf9',
                         'docs/optimization/all-rust/iteration-687-p0-qmi-owner-trace.json': '270ee2124b33a39e6dcffa4fa8e15c1bd3da5314a55575441b95d9f9cd30bb60',
                         'docs/optimization/all-rust/iteration-687-source-refresh-review.md': 'aef0f943436d86021eac78f83fa39b5d3ed114758476e45a04160d49ce343e10',
                         'docs/optimization/all-rust/iteration-687-source-snapshot.json': 'c5bd0ccb93cfb5cb4b7dd1679912407bfd57da7ef0cf213caa2a60c17f77fa5f',
                         'docs/optimization/all-rust/iteration-688-p0-qmi-owner-trace.json': 'f33bd38547af46d60d22b1707c27fbd51e73a744e2e960cf079211c66d1d5dd3',
                         'docs/optimization/all-rust/iteration-688-source-refresh-review.md': '70f2051d03948066730013464d1edcd583c110c7a5c6c1c295c152c609b2a163',
                         'docs/optimization/all-rust/iteration-688-source-snapshot.json': '561f84ed8c4818fe1c6d53e610c9cc0edd19bece307f09eeaad0e4bbaeed465f',
                         'docs/optimization/all-rust/iteration-689-p0-qmi-owner-trace.json': '18ab8684666a13b0f743a9536dcec826b725f9c9a0959d8daa82a79d3c603961',
                         'docs/optimization/all-rust/iteration-689-source-refresh-review.md': '379152ed657ec02ff0d64fb33596a9f8a5bbca2bdd24f02085bf72aeeac34f01',
                         'docs/optimization/all-rust/iteration-689-source-snapshot.json': '43f3e1375ed97f3755580694882473c188b3d72984756b1db7ca6c3d07a06047',
                         'docs/optimization/all-rust/iteration-690-p0-qmi-owner-trace.json': '414eb65e3453be6c2503995ea97007572318dce2ce201ce7c81afd64e8fb1909',
                         'docs/optimization/all-rust/iteration-690-source-refresh-review.md': '85cac5a9f1c89cdcb624c7c3b0fd81414bcae8ac4bcb8c3cf490cdecf0038cbd',
                         'docs/optimization/all-rust/iteration-690-source-snapshot.json': 'bd40f69a823eabef6d5a9f1645dbe5430a335ccba3e094c4f91add5b6f1a5151',
                         'docs/optimization/all-rust/iteration-691-p0-qmi-owner-trace.json': 'de9293c7617de90e0fa53738ad5d7ca310083156e56ad661ab66f8e39eb490dc',
                         'docs/optimization/all-rust/iteration-691-source-refresh-review.md': '679646e5c7db0f9439e3b2440456b9ba32772ea1368adb0e62b487712eb20616',
                         'docs/optimization/all-rust/iteration-691-source-snapshot.json': '75fec8156450f769950f2cfe07407f813f5f127fdd23a40d9e26ff91f15e95d3',
                         'docs/optimization/all-rust/runtime-owner-format-exit-map-iteration-763-pre-successor.json': 'f57a14cb815419b2eed79622bdf5a17e6518f6d93a60eadfc66624cce0d1edcd'},
 'iteration': 764,
 'path': 'docs/optimization/all-rust/iteration-764-source-snapshot.json',
 'previous_file_count': 549,
 'previous_path': 'docs/optimization/all-rust/iteration-691-source-snapshot.json',
 'previous_sha256': '75fec8156450f769950f2cfe07407f813f5f127fdd23a40d9e26ff91f15e95d3'}
SOURCE_SUCCESSOR_CONSERVATION = {'conserved_payload_sha256': {'baseline_provenance': 'd0f704254c2df09c4cb69a798253c9e5bbff9c6dc4749ae1e8d79bc97b0bee4e',
                              'classes': 'cf6e5868bfd3af28653ec4c801ab1a050215fcc9606eac9a5abdda2aebdb26e7',
                              'historical_node_identity_records': 'b82ae87ac1ac7307005948b5c14024e39a3bd0403d918b6d6a9b95ea64dfc6c6',
                              'leaves': 'b81254d782d5d423f87bef46307e1c728b6575a43d81cbcab60e60f8845a7c29',
                              'parents': '1ebf9867bf6d118c606d137351ff25459c16c4fecb6af71a411d6d7781d2a48b',
                              'policies': 'fb50d6b6526ebb4da95d3e77aafa8789aee273d5660b95bf5569eacb0555bb72',
                              'root_adoption': '4d1c42fcdae4632ddd2fa3294c08cf1d0d10f8eda76aa4511dcd0df85b6e7916',
                              'runtime_family_identities': '8fb7f0c45eab4dc2ae84618704d8733b592257c04b3cc5b3b18c856b02a6bf96',
                              'scope_protocol': 'd100ad651000666e5979ddc0db886d18f5dbd7447de01246213980990193c132'},
 'original_events_sha256': '7e503bd9d39b46adac45f83d0f7cc8a740b8f1fe3c5f8384e384033adc514192',
 'path': 'docs/optimization/all-rust/iteration-764-ledger-source-conservation.json',
 'sha256': '917b619a48a8f5204eb02cf46cfd706cacb37a0790fb53c9098a06b6c12a16f2'}
P0_QMI_RECEIVE_POLICY_DEPENDENCY = {'end_line': 77,
 'exact_fragment': 'const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(5);\n'
                   'const BODY_READ_TIMEOUT: Duration = Duration::from_secs(5);\n'
                   '\n'
                   '#[derive(Debug, PartialEq, Eq)]\n'
                   'pub(crate) enum BodyReadError {\n'
                   '    Timeout,\n'
                   '    TooLarge,\n'
                   '    Invalid,\n'
                   '}\n'
                   '\n'
                   '/// The entire ingress shares one absolute deadline, including empty frames '
                   'and\n'
                   '/// tiny chunks. Only collection is cancelled: callers dispatch work '
                   'afterwards.\n'
                   'pub(crate) async fn collect_body<B>(\n'
                   '    body: B,\n'
                   '    maximum_bytes: usize,\n'
                   ') -> Result<bytes::Bytes, BodyReadError>\n'
                   'where\n'
                   '    B: hyper::body::Body<Data = bytes::Bytes> + Unpin,\n'
                   '{\n'
                   '    collect_body_with_timeout(body, maximum_bytes, BODY_READ_TIMEOUT).await\n'
                   '}\n'
                   '\n'
                   'pub(crate) async fn collect_body_with_timeout<B>(\n'
                   '    mut body: B,\n'
                   '    maximum_bytes: usize,\n'
                   '    budget: Duration,\n'
                   ') -> Result<bytes::Bytes, BodyReadError>\n'
                   'where\n'
                   '    B: hyper::body::Body<Data = bytes::Bytes> + Unpin,\n'
                   '{\n'
                   '    use http_body_util::BodyExt;\n'
                   '    let deadline = tokio::time::Instant::now() + budget;\n'
                   '    let mut collected = Vec::new();\n'
                   '    let mut frames_since_yield = 0;\n'
                   '    loop {\n'
                   '        // timeout_at alone may poll an always-ready stream first. Checking '
                   'the\n'
                   '        // clock also bounds a peer producing endless empty/ready frames.\n'
                   '        if tokio::time::Instant::now() >= deadline {\n'
                   '            return Err(BodyReadError::Timeout);\n'
                   '        }\n'
                   '        let Some(frame) = tokio::time::timeout_at(deadline, body.frame())\n'
                   '            .await\n'
                   '            .map_err(|_| BodyReadError::Timeout)?\n'
                   '        else {\n'
                   '            return Ok(bytes::Bytes::from(collected));\n'
                   '        };\n'
                   '        let frame = frame.map_err(|_| BodyReadError::Invalid)?;\n'
                   '        if let Ok(data) = frame.into_data() {\n'
                   '            if data.len() > maximum_bytes.saturating_sub(collected.len()) {\n'
                   '                return Err(BodyReadError::TooLarge);\n'
                   '            }\n'
                   '            collected.extend_from_slice(&data);\n'
                   '        }\n'
                   '        frames_since_yield += 1;\n'
                   '        if frames_since_yield == 64 {\n'
                   '            tokio::task::yield_now().await;\n'
                   '            frames_since_yield = 0;\n'
                   '        }\n'
                   '    }\n'
                   '}\n'
                   '\n'
                   'pub(crate) fn body_timeout_response() -> '
                   'hyper::Response<http_body_util::Full<bytes::Bytes>> {\n'
                   '    crate::json_response(\n'
                   '        hyper::StatusCode::REQUEST_TIMEOUT,\n'
                   '        &serde_json::json!({\n'
                   '            "error":"请求体接收超时，操作尚未提交",\n'
                   '            "code":"request_body_timeout",\n'
                   '            "retryable":true\n'
                   '        }),\n'
                   '    )\n'
                   '}\n'
                   '\n',
 'file': 'rust/crates/proletariusd/src/http_server.rs',
 'line': 6,
 'new_functional_passes': 0,
 'scope': 'shared absolute receive deadline and408 pre-dispatch response; never times out accepted '
          'device work',
 'sha256': '728beb4ddc1a83fb0312698d6e612650fa5f89c0ad39e988168a06ab2ce1f047'}
SOFTWARE_ONLY_SCOPE = {
    "iteration": 666,
    "authority": "DIRECT_USER_INSTRUCTION_FOR_THIS_ROUND",
    "status": "SOFTWARE_ONLY_NO_DEVICE_OR_CARRIER_ACCEPTANCE",
    "connect_dji_module": False,
    "connect_reader": False,
    "physical_or_carrier_acceptance": "UNVERIFIED_EXCLUDED_BY_USER",
    "excluded_acceptance_groups": [
            "DJI_USB_QMI_AT_hardware",
            "PCSC_BLE_eSIM_hardware",
            "carrier_VoWiFi_SMS_call_interoperability",
            "physical_audio_permissions_quality_and_long_run",
            "external_Telegram_Bark_delivery",
            "legacy_3_42_downgrade_support"
    ],
    "physical_or_carrier_leaves_promoted": False,
    "new_functional_passes": 0,
    "overall_denominator_frozen": False,
    "overall_percentage": None,
}
P0_QMI_ROUTE_AUTH_PAIRS = {
    "private_unix_socket": ["main_socket_match", "main_socket_auth", "main_socket_dispatch"],
    "public_and_rs_denied": ["main_public_private_reject"],
}
P0_QMI_ROUTE_ANCHORS = {'main_public_private_reject': (2019,
                                2026,
                                'if path == "/api/internal" || path.starts_with("/api/internal/")'),
 'main_socket_auth': (1801, 1806, 'internal_authenticated'),
 'main_socket_dispatch': (1925, 1926, 'qmi_enhancement::handle'),
 'main_socket_match': (1767, 1767, 'qmi_enhancement::PREFIX')}
P0_QMI_ROLES = {'exit': 'Typed actions hold the QMI operation gate and state mutex through their result; private '
         'close fences retry. Session close aggregates IMS/NAS/UIM and USB client result; client '
         'stops transport and joins reader; retry lifecycle stops/joins worker; main retains '
         'report. Unknown physical cleanup blocks reopen.',
 'format': 'POST JSON is limited to 16 KiB and one shared5s absolute pre-dispatch receive '
           'deadline. Timeout returns408 request_body_timeout without dispatch; other collection '
           'errors return413 and invalid JSON400. Open has a typed control_path; close, probe and '
           'operator require an empty object; set-ims denies unknown fields and an empty update. '
           'QMI core encodes service/client/transaction/message/TLV packets before a serialized '
           'transport write.',
 'producer': 'Swift defaults to -backend=at and has no direct QMI RPC call in the reviewed Swift '
             'source. Explicit QMI mode exposes five typed QMI actions only through the '
             'authenticated private .in Unix socket; public and .rs listeners reject bare '
             '/api/internal and every /api/internal/ descendant before token admission. Main '
             'retains the optional retry worker only in QMI mode. Default AT launch does not prove '
             'QMI reachability; other internal consumers remain unresolved.',
 'writer': 'QmiState mutex serializes session selection/publication; macOS libusb claims one '
           'candidate and writes QMI frames to selected bulk endpoint after CTL_SYNC.'}
P0_QMI_TYPED_ANCHORS = {'ims_read_writer': ('rust/crates/proletarius-qmi-services/src/client.rs',
                     72,
                     85,
                     'IMS_GET_SERVICES'),
 'ims_set_writer': ('rust/crates/proletarius-qmi-services/src/client.rs',
                    87,
                    95,
                    'IMS_SET_SERVICES'),
 'ims_tlv_writer': ('rust/crates/proletarius-qmi-services/src/client.rs',
                    213,
                    233,
                    'CodecError::EmptyUpdate'),
 'imsa_read_writer': ('rust/crates/proletarius-qmi-services/src/client.rs',
                      98,
                      102,
                      'IMSA_GET_REGISTRATION'),
 'nas_name_writer': ('rust/crates/proletarius-qmi-services/src/client.rs',
                     121,
                     135,
                     'NAS_GET_OPERATOR_NAME'),
 'operation_gate': ('rust/crates/proletariusd/src/qmi_enhancement.rs',
                    321,
                    328,
                    'admit_operation("rust-native-qmi")'),
 'owner_operator': ('rust/crates/proletariusd/src/qmi_enhancement.rs',
                    525,
                    527,
                    'session.operator()'),
 'owner_probe': ('rust/crates/proletariusd/src/qmi_enhancement.rs', 487, 491, 'session.probe()'),
 'owner_read_recovery': ('rust/crates/proletariusd/src/qmi_enhancement.rs',
                         535,
                         587,
                         'one QMI recovery attempt also failed'),
 'owner_set_ims': ('rust/crates/proletariusd/src/qmi_enhancement.rs',
                   493,
                   523,
                   'state.reopen(self.factory.as_ref(), &self.module_recovery)'),
 'platform_operator': ('rust/crates/proletariusd/src/qmi_enhancement.rs',
                       858,
                       872,
                       'get_operator_name()'),
 'platform_probe': ('rust/crates/proletariusd/src/qmi_enhancement.rs',
                    820,
                    853,
                    'get_registration_status()'),
 'platform_set_ims': ('rust/crates/proletariusd/src/qmi_enhancement.rs',
                      854,
                      856,
                      'self.ims.set_services(update)'),
 'private_shutdown_gate': ('rust/crates/proletariusd/src/main.rs',
                           1812,
                           1838,
                           'path == "/api/internal/qmi/close"'),
 'service_response_validation': ('rust/crates/proletarius-qmi-services/src/client.rs',
                                 142,
                                 165,
                                 'response.check_result()?'),
 'strict_empty_object': ('rust/crates/proletariusd/src/qmi_enhancement.rs',
                         990,
                         996,
                         'serde_json::Map::is_empty'),
 'swift_default_at_launch': ('app/Sources/BackendProcess.swift', 251, 261, '"-backend=at"'),
 'typed_conversion': ('rust/crates/proletariusd/src/qmi_enhancement.rs',
                      294,
                      311,
                      'impl From<SetImsRequest>'),
 'typed_dispatch': ('rust/crates/proletariusd/src/qmi_enhancement.rs', 950, 964, '"set-ims"'),
 'typed_request': ('rust/crates/proletariusd/src/qmi_enhancement.rs',
                   245,
                   274,
                   'struct SetImsRequest'),
 'typed_response': ('rust/crates/proletariusd/src/qmi_enhancement.rs',
                    971,
                    986,
                    'StatusCode::SERVICE_UNAVAILABLE'),
 'uim_ad_read_fallback': ('rust/crates/proletarius-qmi-services/src/uim.rs',
                          107,
                          110,
                          'unwrap_or_default()'),
 'uim_file_attributes': ('rust/crates/proletarius-qmi-services/src/uim.rs',
                         149,
                         158,
                         'validate_response(&response, SERVICE_UIM, UIM_GET_FILE_ATTRIBUTES)?'),
 'uim_mnc_length_default': ('rust/crates/proletarius-qmi-services/src/uim.rs',
                            357,
                            362,
                            '        2'),
 'uim_plmn_writer': ('rust/crates/proletarius-qmi-services/src/uim.rs',
                     64,
                     69,
                     'read_native_plmn_from_imsi()'),
 'uim_read_record': ('rust/crates/proletarius-qmi-services/src/uim.rs',
                     160,
                     182,
                     'validate_response(&response, SERVICE_UIM, UIM_READ_RECORD)?'),
 'uim_read_transparent': ('rust/crates/proletarius-qmi-services/src/uim.rs',
                          195,
                          213,
                          'validate_response(&response, SERVICE_UIM, UIM_READ_TRANSPARENT)?')}
P0_QMI_TYPED_OPERATIONS = {
    "probe": {
        "route": "POST /api/internal/qmi/probe",
        "request": "Strict empty JSON object, POST only, at most 16 KiB; invalid input performs no typed QMI call.",
        "writer": "Under one state mutex and operation gate, IMS_GET_SERVICES then IMSA_GET_REGISTRATION use the shared QMI client and serialized USB bulk writer.",
        "response": "200 returns settings and registration JSON only after both reads; invalid input is 400, owner/read failure is 503.",
        "failure": "A classified read failure may abort/close the old session, reopen once and re-query; failed or unconfirmed cleanup fences replacement.",
        "exit": "Private close, retry shutdown and daemon shutdown share the retained session close/client join/USB release chain.",
        "typed_unknown": "No physical IMS/IMSA response, registration or DJI transport receipt is proven by this static trace.",
        "anchor_ids": {"main_socket_match", "main_socket_auth", "main_socket_dispatch", "main_public_private_reject", "private_shutdown_gate", "typed_dispatch", "typed_response", "strict_empty_object", "operation_gate", "owner_probe", "owner_read_recovery", "platform_probe", "ims_read_writer", "imsa_read_writer", "service_response_validation", "qmi_wire_writer", "usb_bulk_writer", "platform_session_close", "core_close_join", "usb_release", "retry_shutdown_join", "main_qmi_shutdown"},
    },
    "set-ims": {
        "route": "POST /api/internal/qmi/set-ims",
        "request": "Typed JSON optional IMS fields with deny_unknown_fields and at least one field, POST only, at most 16 KiB.",
        "writer": "Under one state mutex and operation gate, ImsSettingsUpdate becomes IMS_SET_SERVICES TLVs and uses the shared QMI client and serialized USB bulk writer.",
        "response": "200 updated=true only after a validated IMS response; invalid input is 400 and owner/service failure is 503.",
        "failure": "A failure can trigger close/reopen but the IMS mutation is not replayed; a transport error after submission does not prove whether the modem applied it.",
        "exit": "Private close, retry shutdown and daemon shutdown share the retained session close/client join/USB release chain; uncertain cleanup fences reopen.",
        "typed_unknown": "SUBMITTED_MUTATION_OUTCOME_UNKNOWN_ON_LOST_RESPONSE; no physical IMS setting/readback receipt is proven.",
        "anchor_ids": {"main_socket_match", "main_socket_auth", "main_socket_dispatch", "main_public_private_reject", "private_shutdown_gate", "typed_request", "typed_conversion", "typed_dispatch", "typed_response", "operation_gate", "owner_set_ims", "platform_set_ims", "ims_tlv_writer", "ims_set_writer", "service_response_validation", "qmi_wire_writer", "usb_bulk_writer", "platform_session_close", "core_close_join", "usb_release", "retry_shutdown_join", "main_qmi_shutdown"},
    },
    "operator": {
        "route": "POST /api/internal/qmi/operator",
        "request": "Strict empty JSON object, POST only, at most 16 KiB; invalid input performs no typed QMI call.",
        "writer": "Under one state mutex and operation gate, UIM file-attribute, record and transparent reads validate their service responses; OPL falls back to IMSI/EF-AD for MCC/MNC. NAS operator-name read is optional through the shared QMI client and serialized USB bulk writer.",
        "response": "200 returns mcc/mnc/name after UIM success; EF-AD read failure becomes empty AD and can default MNC length to 2, while failed NAS name read becomes an empty name. Invalid input is 400 and UIM/owner failure is 503.",
        "failure": "A classified UIM read failure may abort/close the old session, reopen once and re-query; failed or unconfirmed cleanup fences replacement.",
        "exit": "Private close, retry shutdown and daemon shutdown share the retained session close/client join/USB release chain.",
        "typed_unknown": "EF-AD read failure can be hidden by empty AD and default MNC length 2; NAS name failure can be hidden by an empty name. No physical UIM/NAS result or carrier identity is proven.",
        "anchor_ids": {"main_socket_match", "main_socket_auth", "main_socket_dispatch", "main_public_private_reject", "private_shutdown_gate", "typed_dispatch", "typed_response", "strict_empty_object", "operation_gate", "owner_operator", "owner_read_recovery", "platform_operator", "uim_plmn_writer", "uim_ad_read_fallback", "uim_file_attributes", "uim_read_record", "uim_read_transparent", "uim_mnc_length_default", "nas_name_writer", "service_response_validation", "qmi_wire_writer", "usb_bulk_writer", "platform_session_close", "core_close_join", "usb_release", "retry_shutdown_join", "main_qmi_shutdown"},
    },
}
P0_QMI_NEW_SOURCE_FILES = set()
P0_QMI_CHANGED_SOURCE_FILES = {'rust/crates/proletariusd/src/main.rs', 'rust/crates/proletariusd/src/qmi_enhancement.rs'}
P0_QMI_CURRENT_ADDED_ANCHORS = set()
P0_QMI_INDEPENDENT_PINS = {
    "rust/crates/proletarius-qmi-services/src/client.rs",
    "rust/crates/proletarius-qmi-services/src/uim.rs",
}
P0_QMI_SOURCE_REFRESH_REVIEW = ('Successor to immutable i691 nine-file finite QMI trace on final F2 business source2c11207 and '
 'approved source census4cc82b. Manually reviewed main whole production delta and actual '
 'route/auth dispatch bodies:751 adds network coordinator shutdown/drain; QMI route bodies/owners '
 'remain unchanged. Six main anchors relocate by unique complete exact text. The QMI receive '
 'boundary explicitly changes from unbounded Limited.collect to the existing shared5s absolute '
 'collector: Timeout returns408 request_body_timeout before JSON parsing/typed dispatch; other '
 'collection errors remain413 and invalid JSON400. The collector dependency is independently '
 'source-pinned. No QMI hardware/carrier, producer completeness, runtime result, functional '
 'acceptance or denominator freeze is claimed; all immutable predecessors and exclusions remain.')
P0_QMI_UNRESOLVED = ['Historical macOS QMI/MBIM supported models, firmware and operations are not recoverable from this current-code trace alone.', 'This static successor audit establishes no QMI CTL_SYNC/service/USB cleanup or hotplug execution. The i667 DJI/CTExcel AT-based IMS registration subset does not validate the QMI transport or other hardware/carrier combinations.', 'Swift now defaults to -backend=at and projects /api/status, with no direct call to the reviewed private QMI routes. Explicit QMI consumers, default-launch QMI reachability and historical support membership remain UNRESOLVED_NOT_PASS.', 'Set-ims can have an UNKNOWN modem outcome after a submitted request loses its response; this static trace does not authorize replay or award a successful setting.', 'Operator EF-AD read failure can be hidden by empty AD and a default MNC length of 2; NAS name read failure can be hidden by an empty name, so 200 does not prove those reads or carrier identity.', 'Other P0 producer/writer/exit families, UIM business scope and the formal device/card/carrier instance registry remain outside this finite slice.']
P0_QMI_BRANCHES = {
    "at_mode_no_retry": (
        "-backend=at", "No QMI retry worker or physical QMI session is started.",
        {"retry_worker"}),
    "auto_absent_retry": (
        "QMI mode, automatic selection, no matching device",
        "Owner records unavailable/backoff; a retained worker retries without publishing a session.",
        {"main_retry_producer", "retry_entry", "configured_open_writer", "selector_auto_exact"}),
    "auto_claim_ctl": (
        "QMI mode, matching interface and successful CTL_SYNC",
        "One claimed transport/client plus IMS/NAS/UIM session is published.",
        {"usb_claim", "usb_ctl_sync", "macos_factory", "configured_open_writer"}),
    "exact_selector_reconnect": (
        "Private open with exact VID:PID; link later disappears",
        "Retained selector/epoch constrain reconnect after confirmed old-session close.",
        {"main_socket_match", "main_socket_auth", "main_socket_dispatch", "main_public_private_reject",
         "http_open_close_format", "private_open_selector",
         "selector_auto_exact", "retry_reconnect"}),
    "private_close_fence": (
        "Authenticated private close during idle or retry",
        "Explicit-close epoch prevents automatic reopen; close error remains observable.",
        {"main_socket_match", "main_socket_auth", "main_socket_dispatch", "main_public_private_reject",
         "private_close_fence", "retry_entry", "platform_session_close"}),
    "late_open_cancellation": (
        "Startup timeout, private close, or daemon stop during open",
        "Late handoff cannot orphan an owned session; worker and cleanup result are joined.",
        {"late_handoff", "retry_shutdown_join", "main_qmi_shutdown"}),
    "detach_cleanup_unknown": (
        "Reader loss or physical release error",
        "Old client/USB release is attempted; unconfirmed result fences subsequent auto open.",
        {"retry_reconnect", "core_close_join", "usb_release", "configured_open_writer",
         "reopen_cleanup_fence", "platform_abort_result"}),
    "shutdown_join": (
        "Daemon shutdown with QMI startup/retry still running",
        "Stop and join result is returned to main shutdown report, including unknown close.",
        {"main_qmi_shutdown", "retry_shutdown_join", "core_close_join"}),
}
AXES = {"normal", "cancel", "partial", "durable", "release"}
STATUSES = {"NOT_REVIEWED", "UNTESTED", "PARTIAL", "FAIL", "PASS", "NOT_APPLICABLE"}
FACET_STATUSES = STATUSES | {"CANDIDATE_SOFTWARE_SUBSET"}
# Pins the existing requirement predicates, not a claim of inventory completeness
# or a denominator. Intentional scope changes need a separately reviewed version.
# i652 changes only the existing NINE_ESIM class scope after the later direct
# request supersedes the i614 exclusion. The prior catalog remains historical:
# 0b740c221856d58882812527f8d21979bfec263a56d129b8dbd24f699714bcd6.
# The predecessor catalog is preserved byte-for-byte in the i680
# pre-activation ledger.  The only reviewed predicate change is the
# one-to-one P0-1 current-baseline scope replacement; see the separate
# activation checker for its full old/new and 113-leaf raw-byte proof.
RETAINED_CATALOG_PIN_PRE_ACTIVATION = "b246fb5ee4776b5e3b30f062fa646f9b0a45bc7b7c7e256402ce9377184f3d21"
RETAINED_CATALOG_PIN = "e7840bea45acd44349ecf0c86865d71cd07d140a7fd77338c9e8dc144f20d7af"
P0_CURRENT_BASELINE_LEAVES = {
    "P0-1.current-source-baseline", "P0-1.current-build-baseline"
}
P0_ACTIVATION_REF = {
    "path": "docs/optimization/all-rust/p0-1-current-baseline-activation-2026-10-23.json",
    "sha256": "93275630888c7c827b1672cf5678ad1e0c5257adfda2d8fd20cbd89778df4ee4",
}
# Reviewed against the SHA-pinned i597 adoption candidate. Keep this source-only:
# cold build capsules export src and offline dependencies, not sibling reports.
ROOT_ADOPTION_CANDIDATE = {
    "name": "iteration-597-runtime-owner-format-exit-map-prepared-v2.json",
    "sha256": "f3ccf2d06ca6a3abe6075196266685b03ee6dcff88b432a6aca2abbd1ad3b3c7",
    "historical_source_node_count": 342,
    "status_counts": {
        "UNIQUE_EXACT_FRAGMENT_REBOUND": 65,
        "SUPERSEDED_OR_REANCHOR_REQUIRED": 14,
        "EXACT_WHOLE_FILE_RAW_MATCH": 263,
    },
    "checks": {"bound": 328, "pending": 14, "new": 15},
}
NINE_ESIM_CURRENT_SCOPE = {
    "iteration": 652,
    "status": "REOPENED_DISPUTED_SCOPE_NOT_NA_NOT_PASS",
    "requested_authentication": ["B0/B1 password", "B0 BF71"],
    "authorized_normal_download_assessment": "IN_SCOPE_UNRESOLVED",
    "6982_diagnosis": "IN_SCOPE_UNRESOLVED",
    "protected_download_applicability": "UNRESOLVED_REQUIRES_AUTHORIZED_CAPABILITY_EVIDENCE",
    "earlier_exclusion_current": False,
    "authentication_bypass_allowed": False,
    "6982_fixed_claimed": False,
    "functional_acceptance_awarded": False,
}


def digest_bytes(data):
    return hashlib.sha256(data).hexdigest()


def canonical_digest(value):
    return digest_bytes(json.dumps(value, ensure_ascii=False, sort_keys=True,
                                   separators=(",", ":")).encode())


def file_digest(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def safe_source(root, relative):
    path = Path(relative)
    if path.is_absolute() or not path.parts or ".." in path.parts:
        raise ValueError("unsafe source path: " + str(relative))
    resolved = (root / path).resolve(strict=True)
    if root.resolve() not in resolved.parents or not resolved.is_file():
        raise ValueError("source path escapes root or is not a file: " + str(relative))
    return resolved


def runtime_rows(text):
    rows = []
    for line_number, line in enumerate(text.splitlines(), 1):
        match = re.match(r"\| (RT-[A-Z]\d+)\s", line)
        if match:
            rows.append({"id": match.group(1), "line": line_number, "raw": line})
    return rows


def retained_catalog(ledger):
    return {
        "parents": ledger["parents"],
        "leaves": [{**{k: row[k] for k in (
            "id", "parent_id", "stage", "title", "mandatory", "requirement_sources", "pass_predicate")},
            "axes": {axis: {k: facet[k] for k in ("applicability", "required_condition")}
                     for axis, facet in row["evidence"].items()},
            "scenario_class_ids": row["applicability"]["scenario_class_ids"]}
            for row in ledger["leaves"]],
        "classes": [{k: row[k] for k in ("id", "kind", "committed_requirement_scope")}
                    for row in ledger["applicability_matrix"]],
        "policy_ids": [row["id"] for row in ledger["unresolved_policies"]],
    }


def resolve_reference(bundle, reference):
    name, marker, pointer = reference.partition("#/")
    names = {value: key for key, value in FILES.items()}
    if not marker or name not in names:
        raise ValueError("unknown scope reference: " + reference)
    value = bundle[names[name]]
    for encoded in pointer.split("/"):
        key = encoded.replace("~1", "/").replace("~0", "~")
        value = value[int(key)] if isinstance(value, list) else value[key]
    return value


def _normalized_lines(text):
    return [line.strip() for line in text.splitlines()]


def source_ref_errors(mapping, root):
    """Check that every non-historical source reference names current bytes."""
    errors = []
    pins = {}
    for group in ("current_source_file_pins", "supplementary_reviewed_source_file_pins"):
        for pin in mapping.get(group, []):
            pins[pin.get("file")] = pin.get("sha256")

    def visit(value, location):
        if isinstance(value, dict):
            if {"file", "line", "source_fragment", "sha256"} <= set(value):
                relative = value["file"]
                try:
                    path = safe_source(root, relative)
                except (TypeError, ValueError, OSError):
                    errors.append(location + ": unsafe or missing source reference")
                    return
                expected_pin = pins.get(relative)
                if expected_pin is None:
                    errors.append(location + ": source reference is not pinned: " + relative)
                elif value["sha256"] != expected_pin:
                    errors.append(location + ": source reference does not use the current pin: " + relative)
                line = value["line"]
                if type(line) is not int or line < 1:
                    errors.append(location + ": invalid source reference line")
                else:
                    lines = path.read_text().splitlines()
                    fragment = value["source_fragment"].splitlines()
                    actual = lines[line - 1:line - 1 + len(fragment)]
                    if not fragment or _normalized_lines("\n".join(actual)) != _normalized_lines("\n".join(fragment)):
                        errors.append(location + ": source fragment differs at its declared line: " + relative)
            for key, child in value.items():
                # A nested original_source_ref is deliberately retained as a
                # historical record and must not be rebound to current bytes.
                if key != "original_source_ref":
                    visit(child, location + "/" + str(key))
        elif isinstance(value, list):
            for index, child in enumerate(value):
                visit(child, location + "[" + str(index) + "]")

    for catalog in ("API_entries", "Swift_call_entries", "RT_entries", "reviewed_resolution_entries"):
        visit(mapping.get(catalog, []), catalog)
    return errors


def validate_bundle(bundle, rt_rows, root=ROOT):
    """Pure metadata checks, also used by mutation tests. No PASS is written."""
    errors = []

    def require(condition, message):
        if not condition:
            errors.append(message)

    ledger, mapping = bundle["ledger"], bundle["mapping"]
    require(ledger.get("review_update", {}).get("software_acceptance_scope") == SOFTWARE_ONLY_SCOPE,
            "current software-only scope changed or claims physical/carrier acceptance")
    protocol, baselines, inventory = (bundle[k] for k in ("protocol", "baselines", "inventory"))
    leaves = ledger["leaves"]
    leaf_ids = [row["id"] for row in leaves]
    leaf_by_id = {row["id"]: row for row in leaves}
    parents = [row["id"] for row in ledger["parents"]]
    classes = [row["id"] for row in ledger["applicability_matrix"]]
    policies = [row["id"] for row in ledger["unresolved_policies"]]
    require(canonical_digest(retained_catalog(ledger)) == RETAINED_CATALOG_PIN,
            "retained predicates/classes/policies changed without a reviewed catalog version")
    require(set(protocol["P0_normal_rules"]) ==
            {row["id"] for row in leaves if row["stage"] == "P0"},
            "active P0 review rules differ from active P0 leaves")
    require({row["id"] for row in leaves if row["parent_id"] == "P0-1"}
            == P0_CURRENT_BASELINE_LEAVES,
            "current-baseline P0-1 scope replacement differs")
    require(ledger.get("scope_activation") == {
                "status": "ACTIVE_SCOPE_RULE_NO_FUNCTIONAL_ACCEPTANCE",
                "decision": P0_ACTIVATION_REF,
                "historical_p0_leaf_results_carried_forward": False,
                "other_113_leaf_objects_unchanged": True,
                "overall_denominator_frozen": False,
                "overall_percentage": None,
            } and protocol.get("P0_scope_activation", {}).get("decision_ref")
            == P0_ACTIVATION_REF,
            "P0-1 activation authority or no-pass boundary changed")
    for rows, group in ((ledger["applicability_matrix"], "scenario_protocols"),
                        (ledger["unresolved_policies"], "policy_decisions")):
        for row in rows:
            try:
                resolve_reference(bundle, row["scope_protocol_ref"])
                require(row["scope_protocol_ref"] == "acceptance-scope-protocol.json#/" + group + "/" + row["id"],
                        row["id"] + ": scope reference targets a different identity")
            except (KeyError, ValueError, IndexError, TypeError):
                errors.append(row["id"] + ": invalid scope protocol reference")
    for row in leaves:
        if row["stage"] == "P0":
            try:
                resolved = resolve_reference(bundle, row["normal_review_rule_ref"])
                require(resolved["existing_leaf_id"] == row["id"], row["id"] + ": P0 review rule identity differs")
                require(resolved.get("normal_required_condition_unchanged") ==
                        row["evidence"]["normal"]["required_condition"],
                        row["id"] + ": P0 normal predicate differs from adopted rule")
            except (KeyError, ValueError, IndexError, TypeError):
                errors.append(row["id"] + ": invalid P0 review rule reference")
    for label, values in (("leaf", leaf_ids), ("parent", parents), ("class", classes), ("policy", policies)):
        require(len(values) == len(set(values)), "duplicate " + label + " ID")
    require(set(parents).isdisjoint(leaf_ids), "parents must not be scored leaves")
    require(set(mapping["required_leaf_ids"]) == set(leaf_ids), "mapping requirement set differs from ledger")
    require(mapping["required_leaf_ids"] == sorted(set(mapping["required_leaf_ids"])), "requirement IDs not canonical unique order")
    for leaf in leaves:
        label = leaf["id"]
        require(leaf["parent_id"] in parents, label + ": unknown parent")
        require(set(leaf["evidence"]) == AXES, label + ": evidence axes differ")
        require(leaf["mandatory"] is True, label + ": retained requirement silently made optional")
        require(leaf["acceptance_status"] in STATUSES - {"NOT_APPLICABLE"}, label + ": invalid acceptance status")
        require(set(leaf["applicability"]["scenario_class_ids"]) <= set(classes), label + ": unknown scenario class")
        require(set(leaf["unresolved_policy_ids"]) <= set(policies), label + ": unknown policy")
        for axis, facet in leaf["evidence"].items():
            applicable = facet["applicability"]
            require(applicable in {"REQUIRED", "NOT_APPLICABLE"}, label + ": invalid applicability")
            require(facet["status"] in FACET_STATUSES, label + ": invalid evidence status")
            if facet["status"] == "CANDIDATE_SOFTWARE_SUBSET":
                require(bool(facet["evidence_refs"]) and leaf["acceptance_status"] == "PARTIAL",
                        label + ": software subset needs evidence and a PARTIAL leaf")
            if applicable == "NOT_APPLICABLE":
                require(bool(facet.get("not_applicable_reason")), label + ": unexplained N/A " + axis)
                require(facet["status"] == "NOT_APPLICABLE", label + ": inconsistent N/A " + axis)
            else:
                require(bool(facet.get("required_condition")), label + ": missing condition " + axis)
                require(facet["status"] != "NOT_APPLICABLE", label + ": required axis hidden as N/A")
                if facet["status"] == "PASS":
                    errors.append(label + ": facet PASS requires a separately reviewed instance/evidence registry")
        require(leaf["evidence"]["normal"]["applicability"] == "REQUIRED", label + ": normal not required")
        if leaf["acceptance_status"] == "PASS":
            errors.append(label + ": leaf PASS requires a separately reviewed instance/evidence registry")
            require(bool(leaf["applicability"]["instances"]), label + ": PASS without frozen instances")
            require(all(f["status"] == "PASS" for f in leaf["evidence"].values()
                        if f["applicability"] == "REQUIRED"), label + ": PASS before required facets")

    expected = {"API_entries": inventory["routes"], "Swift_call_entries": inventory["swift_calls"], "RT_entries": rt_rows}
    counts = {}
    source_pins = mapping["current_source_file_pins"]
    require(bool(source_pins), "empty mapping source pins")
    require(len(source_pins) == len({p["file"] for p in source_pins}), "duplicate mapping source pin")
    pin_by_file = {p["file"]: p["sha256"] for p in source_pins}
    for catalog, originals in expected.items():
        rows = mapping[catalog]
        counts[catalog] = len(rows)
        require(len(rows) == len(originals), catalog + ": dropped or added catalog entry")
        indices = [row["original_array_index"] for row in rows]
        require(sorted(indices) == list(range(len(originals))), catalog + ": duplicate or missing source index")
        for row in rows:
            index = row["original_array_index"]
            if type(index) is not int or not 0 <= index < len(originals):
                errors.append(catalog + ": invalid original index")
                continue
            original = originals[index]
            label = catalog + ":" + str(index)
            require(row["original_record_sha256"] == canonical_digest(original), label + ": original catalog record changed")
            if catalog == "API_entries":
                require(row["entry_id"] == original["id"], label + ": route identity changed")
                require(row.get("derived_from") == original.get("alias_of"), label + ": HEAD alias changed")
                if original.get("alias_of"):
                    require(original["alias_of"] in {r["id"] for r in inventory["routes"]}, label + ": missing GET parent")
            elif catalog == "Swift_call_entries":
                key = row["catalog_key"]
                require(type(key.get("array_index")) is int and type(key.get("line")) is int, label + ": non-integer Swift identity")
                require(all(key.get(k) == v for k, v in {
                    "array_index": index, "file": original["source"]["file"],
                    "line": original["source"]["line"], "expression": original["expression"]}.items()), label + ": Swift identity changed")
            else:
                require(row["entry_id"] == original["id"], label + ": RT identity changed")
            require(row.get("independent_business_score") is False, label + ": entry is incorrectly a scoring unit")
            require(row.get("acceptance_pass_awarded") is False, label + ": mapping awards acceptance")
            primary = row.get("primary_leaf_id")
            branches = row.get("primary_by_behavior_branch", [])
            require(primary in leaf_by_id or (primary is None and bool(branches)), label + ": missing/unknown primary")
            require(set(row.get("related_existing_leaf_ids", [])) <= set(leaf_ids), label + ": unknown related leaf")
            require(set(row["applicable_scenario_class_ids"]) <= set(classes), label + ": unknown scenario")
            for branch in branches:
                leaf = leaf_by_id.get(branch["primary_leaf_id"])
                require(leaf is not None, label + ": unknown branch leaf")
                require(bool(branch["condition"]), label + ": unconditional composite branch")
                if leaf:
                    require(set(branch["scenario_class_ids"]) <= set(leaf["applicability"]["scenario_class_ids"]), label + ": branch expands leaf scenario scope")
            permitted_classes = set(leaf_by_id[primary]["applicability"]["scenario_class_ids"]) if primary in leaf_by_id else set()
            for branch in branches:
                permitted_classes.update(branch["scenario_class_ids"])
            require(set(row["applicable_scenario_class_ids"]) <= permitted_classes,
                    label + ": entry expands primary/conditional scenario scope")
            for field in ("owner", "data_format", "exit"):
                resolution = row.get(field)
                if not isinstance(resolution, dict):
                    errors.append(label + ": untyped " + field + " resolution")
                    continue
                state = resolution.get("resolution")
                require(state in {"EXACT", "CATALOG_ONLY", "UNKNOWN", "NOT_APPLICABLE"}, label + ": invalid " + field + " resolution")
                require(bool(resolution.get("claim")), label + ": missing " + field + " claim boundary")
                require(resolution.get("certifies_complete_graph") is False, label + ": unresolved mapping claims a complete graph")
                if state in {"UNKNOWN", "CATALOG_ONLY", "NOT_APPLICABLE"}:
                    require(bool(resolution.get("reason")), label + ": missing " + field + " reason")
                if state == "EXACT":
                    refs = resolution.get("source_refs", [])
                    require(bool(refs) and not resolution.get("candidates"), label + ": EXACT needs pinned source facts, not candidates")
                    for ref in refs:
                        require(ref.get("file") in pin_by_file and pin_by_file.get(ref.get("file")) == ref.get("sha256"), label + ": unpinned EXACT source")
                        require(type(ref.get("line")) is int and ref["line"] > 0 and bool(ref.get("symbol")), label + ": incomplete EXACT source location")

    require(set(protocol["scenario_protocols"]) == set(classes), "scenario protocol does not cover existing classes")
    require(set(protocol["policy_decisions"]) == set(policies), "policy protocol does not cover existing policies")
    for key, definition in protocol["scenario_protocols"].items():
        require(bool(definition.get("finite_protocol_definition")), key + ": empty finite protocol")
        require(bool(definition.get("membership_fact_required")), key + ": missing support fact boundary")
    nine_protocol = protocol["scenario_protocols"].get("NINE_ESIM", {})
    nine_ledger = next((row for row in ledger["applicability_matrix"]
                        if row["id"] == "NINE_ESIM"), {})
    require(nine_protocol.get("current_user_scope_decision") == NINE_ESIM_CURRENT_SCOPE
            and nine_ledger.get("current_user_scope_decision") == NINE_ESIM_CURRENT_SCOPE
            and nine_ledger.get("resolution_status") == NINE_ESIM_CURRENT_SCOPE["status"],
            "NINE_ESIM: later user scope decision drift or unsupported resolution")
    require("including 6982 diagnosis" in nine_protocol.get("finite_protocol_definition", "")
            and "explicitly out of scope" not in nine_protocol.get("finite_protocol_definition", "")
            and "do not investigate" not in nine_protocol.get("membership_fact_required", ""),
            "NINE_ESIM: superseded 6982 exclusion remains active")
    scope_history = protocol.get("user_scope_update_history", [])
    require(all(any(row.get("iteration") == iteration
                        and row.get("status") == "HISTORICAL_SUPERSEDED"
                        and row.get("superseded_by_iteration") == 652 for row in scope_history)
                for iteration in (614, 615)),
            "NINE_ESIM: superseded scope history is missing")
    require(any(row.get("iteration") == 652 and row.get("status") == "CURRENT_UNRESOLVED"
                and row.get("supersedes_iterations") == [614, 615]
                and row.get("current_user_scope_decision") == NINE_ESIM_CURRENT_SCOPE
                and bool(row.get("direct_user_quote")) for row in scope_history),
            "NINE_ESIM: later direct user scope provenance is missing")
    for key, decision in protocol["policy_decisions"].items():
        require(bool(decision.get("decision")), key + ": missing concrete decision")
        require("remaining_facts" in decision, key + ": unknown facts not explicit")
        current = next((p for p in ledger["unresolved_policies"] if p["id"] == key), {})
        require(current.get("resolution") == decision.get("decision") and
                current.get("status") == decision.get("status") and
                current.get("remaining_facts") == decision.get("remaining_facts"), key + ": ledger/protocol policy drift")
        if decision.get("remaining_facts"):
            require(current.get("blocks_overall_denominator") is True, key + ": missing facts hidden from denominator")
    adopted_environment_classes = {
        "CLEAN": ("clean_environment", ("CLEAN-BUILD", "CLEAN-INSTALL")),
        "MEASURE": ("performance_budget", ("§6.2", "整机测量")),
        "LONGRUN": ("final_soak", ("≥72h", "≥3")),
    }
    class_by_id = {row["id"]: row for row in ledger["applicability_matrix"]}
    for class_id, (policy_id, required_terms) in adopted_environment_classes.items():
        row = class_by_id.get(class_id, {})
        decision = protocol["policy_decisions"].get(policy_id, {})
        pending_work = row.get("required_resolution", "")
        require(decision.get("status") == "RULE_ADOPTED_EXECUTION_PENDING"
                and decision.get("remaining_facts") == []
                and row.get("resolution_status") == "ENGINEERING_PROTOCOL_DEFINED_SUPPORT_MEMBERSHIP_PENDING"
                and all(term in pending_work for term in required_terms)
                and "已采纳" in pending_work
                and not any(stale in pending_work for stale in ("定义待决", "预算待定", "待采纳")),
                class_id + ": adopted protocol is described as undecided in ledger applicability")
    require(protocol["unit_identity_fields"] == ["leaf_id", "scenario_class_id", "semantic_instance_id"], "unit identity changed")
    require(protocol["evidence_is_not_unit_identity"] is True, "binary/alias/evidence incorrectly defines a new unit")
    require(set(baselines["roles"]) == {"seed_263", "pre_280", "final_280", "pre_all_rust", "current"}, "historical baseline roles missing or conflated")
    seed = baselines["roles"].get("seed_263", {})
    require(seed.get("may_close_leaf_ids") == [], "seed must not close switch-time/2.80 baseline acceptance")
    require(set(seed.get("cannot_substitute_for", [])) >= {"pre_280", "final_280", "pre_all_rust"}, "seed substitution boundary missing")
    require(seed.get("historical_metadata_completeness_claimed") is False, "seed content recovery claims historical metadata completeness")
    require(baselines["overall_historical_baseline_complete"] is False or not baselines["unresolved_facts"], "historical baseline declared complete with unresolved facts")
    freeze = ledger["freeze"]
    if not freeze["overall_denominator_frozen"]:
        require(freeze["overall_denominator"] is None and freeze["overall_percentage"] is None, "unfrozen denominator/percentage published")
    else:
        # An actual scored instance registry and verified unique unit index must
        # be implemented/reviewed before enabling final counting in this tool.
        # Never substitute 115 leaves, route count or empty instance arrays.
        errors.append("frozen counting is unsupported until a reviewed instance/unit registry is supplied")
    require(mapping["overall_product_complete"] is False, "mapping claims product completion")
    errors.extend(source_ref_errors(mapping, root))
    return {"metadata_consistent": not errors, "errors": errors, "catalog_counts": counts,
            "requirement_leaves": len(leaves), "scenario_classes": len(classes),
            "overall_denominator_frozen": freeze["overall_denominator_frozen"],
            "overall_denominator": freeze["overall_denominator"],
            "overall_percentage": freeze["overall_percentage"],
            "functional_acceptance_awarded": False, "overall_product_complete": False}


def validate_source_pins(mapping, root):
    errors = []
    seen = {}

    def identity(pin):
        digest = pin.get("sha256")
        frozen = pin.get("frozen_cut_sha256", digest)
        present = pin.get("present_at_frozen_cut", True)
        matches = pin.get("matches_frozen_cut", digest == frozen)
        update = pin.get("source_update")
        return digest, frozen, present, matches, canonical_digest(update) if update is not None else None

    for group in ("current_source_file_pins", "supplementary_reviewed_source_file_pins"):
        for pin in mapping.get(group, []):
            relative = pin.get("file")
            pin_identity = identity(pin)
            prior_groups = seen.setdefault(relative, {})
            if group in prior_groups:
                errors.append("duplicate source pin within " + group + ": " + str(relative))
            elif prior_groups and any(value != pin_identity for value in prior_groups.values()):
                errors.append("conflicting source pin across review groups: " + str(relative))
            else:
                prior_groups[group] = pin_identity
            path = safe_source(root, relative)
            current = file_digest(path)
            if current != pin.get("sha256"):
                errors.append("source pin drift: " + relative)
            present = pin.get("present_at_frozen_cut", True)
            source_update = pin.get("source_update")
            if present:
                frozen = pin.get("frozen_cut_sha256", pin.get("sha256"))
                if not re.fullmatch(r"[0-9a-f]{64}", str(frozen)):
                    errors.append("invalid frozen-cut source pin: " + relative)
                matches = pin.get("sha256") == frozen
                if "matches_frozen_cut" in pin and pin["matches_frozen_cut"] is not matches:
                    errors.append("matches_frozen_cut disagrees with pin hashes: " + relative)
                if not matches and not _valid_source_update(source_update):
                    errors.append("changed source pin lacks update provenance: " + relative)
            else:
                if pin.get("matches_frozen_cut") is not False or pin.get("frozen_cut_sha256") is not None:
                    errors.append("post-cut source pin claims frozen-cut membership: " + relative)
                if not _valid_source_update(source_update):
                    errors.append("post-cut source pin lacks provenance: " + relative)
    return errors


def _valid_source_update(update):
    return (isinstance(update, dict)
            and type(update.get("iteration")) is int
            and update["iteration"] > 0
            and bool(update.get("reason"))
            and bool(update.get("evidence_refs")))


def resolve_map_source(root, raw_path):
    path = Path(raw_path)
    if not path.is_absolute():
        return safe_source(root, str(path))
    resolved = path.resolve(strict=True)
    root_resolved = root.resolve()
    if root_resolved in resolved.parents:
        return resolved
    # The committed source map carries the original absolute workspace prefix.
    # Resolve its `src/...` suffix against the explicitly selected checkout.
    parts = path.parts
    for index, part in enumerate(parts):
        if part == "src" and index + 1 < len(parts):
            return safe_source(root, str(Path(*parts[index + 1:])))
    raise ValueError("source map path is outside selected workspace: " + raw_path)


def validate_runtime_source_map(runtime_map, root):
    """Verify current source bindings while retaining historical graph records."""
    errors = []

    def source_fragment_matches(path, start, end, fragment):
        if type(start) is not int or type(end) is not int or start < 1 or end < start:
            return False
        lines = path.read_text().splitlines()
        expected = fragment.splitlines()
        return end - start + 1 == len(expected) and lines[start - 1:end] == expected

    def source_fragment_within_range(path, start, end, fragment):
        if type(start) is not int or type(end) is not int or start < 1 or end < start:
            return False
        lines = path.read_text().splitlines()
        if end > len(lines) or not isinstance(fragment, str) or not fragment:
            return False
        exact_range = "\n".join(lines[start - 1:end])
        return fragment.rstrip("\r\n") in exact_range

    historical_nodes = runtime_map.get("historical_source_nodes", [])
    accounting = runtime_map.get("accounting_not_acceptance", {})
    current_counts = dict(Counter(node.get("current_binding", {}).get("status")
                                  for node in historical_nodes))
    if (accounting.get("historical_source_node_count") != len(historical_nodes)
            or accounting.get("current_binding_status_counts") != current_counts):
        errors.append("runtime current binding status counts differ from historical source rows")
    adoption_counts = accounting.get("current_node_binding_status_counts", {})
    adoption = runtime_map.get("root_adoption", {})
    adoption_checks = adoption.get("root_actual_exact_current_node_checks", {})
    if (accounting.get("current_node_binding_status_counts_scope")
            != "PINNED_ITERATION_597_ROOT_ADOPTION_CANDIDATE"
            or len(historical_nodes) != ROOT_ADOPTION_CANDIDATE["historical_source_node_count"]
            or adoption_counts != ROOT_ADOPTION_CANDIDATE["status_counts"]
            or adoption_checks != ROOT_ADOPTION_CANDIDATE["checks"]):
        errors.append("runtime root-adoption binding tally disagrees with reviewed candidate")
    if (not isinstance(adoption.get("candidate"), str)
            or Path(adoption["candidate"]).name != ROOT_ADOPTION_CANDIDATE["name"]
            or adoption.get("candidate_sha256") != ROOT_ADOPTION_CANDIDATE["sha256"]):
        errors.append("runtime root-adoption candidate identity or SHA pin differs from reviewed source")
    if (accounting.get("existing_requirement_leaves") != 115
            or accounting.get("new_exact_replacement_source_node_count")
            != len(runtime_map.get("new_current_replacement_nodes", []))
            or any(type(accounting.get(key)) is not int or accounting[key] != 0 for key in
                   ("new_requirement_ids", "new_scored_instances", "new_functional_PASS_instances"))
            or accounting.get("overall_denominator") is not None
            or accounting.get("overall_percentage") is not None):
        errors.append("runtime accounting claims scored units, acceptance, or a frozen denominator")

    for index, node in enumerate(historical_nodes):
        binding = node.get("current_binding", {})
        status = binding.get("status")
        location = "historical_source_nodes[" + str(index) + "] " + str(node.get("node_id"))
        if status == "SUPERSEDED_OR_REANCHOR_REQUIRED":
            if binding.get("current_anchor") is not None or binding.get("final_source_snapshot_bound") is not False:
                errors.append(location + ": superseded node still claims a current anchor")
            continue
        if status not in {"EXACT_WHOLE_FILE_RAW_MATCH", "UNIQUE_EXACT_FRAGMENT_REBOUND"}:
            errors.append(location + ": unknown current binding status")
            continue
        anchor = binding.get("current_anchor")
        if not isinstance(anchor, dict):
            errors.append(location + ": current binding is missing its anchor")
            continue
        try:
            path = resolve_map_source(root, anchor["file"])
            actual = file_digest(path)
        except (KeyError, TypeError, ValueError, OSError):
            errors.append(location + ": current source path is invalid")
            continue
        if anchor.get("raw_sha256") != actual or binding.get("current_file_sha256_observed") != actual:
            errors.append(location + ": current source hash is stale")
        if not source_fragment_matches(path, anchor.get("line"), anchor.get("end_line"), anchor.get("exactfragment", "")):
            errors.append(location + ": current anchor fragment/range is stale")
        if status == "EXACT_WHOLE_FILE_RAW_MATCH":
            if actual != node.get("historical", {}).get("raw_sha256"):
                errors.append(location + ": whole-file match status contradicts historical hash")
            relation = anchor.get("historical_fragment_relation")
            historical = node.get("historical", {})
            historical_record = historical.get("source_record", {})
            historical_fragment = historical_record.get("fragment", historical.get("exactfragment", ""))
            current_fragment = anchor.get("exactfragment", "")
            try:
                historical_path = resolve_map_source(root, historical["file"])
                historical_record_path = resolve_map_source(root, historical_record.get("file", historical["file"]))
                if historical_path != path or historical_record_path != path:
                    errors.append(location + ": whole-file relation paths do not identify the same source")
            except (KeyError, TypeError, ValueError, OSError):
                errors.append(location + ": historical source path is invalid")
            historical_start = historical_record.get("line", historical.get("line"))
            historical_end = historical_record.get("end_line", historical.get("end_line", historical_start))
            if (historical_start != historical.get("line") or
                    historical_end != historical.get("end_line", historical_start)):
                errors.append(location + ": historical source-record range disagrees with its node")
            if relation == "EXACT_RANGE_EQUAL":
                if historical_fragment != current_fragment:
                    errors.append(location + ": exact-range relation contradicts historical fragment")
                if historical_start != anchor.get("line") or historical_end != anchor.get("end_line"):
                    errors.append(location + ": exact-range relation contradicts historical range")
                if not source_fragment_matches(path, historical_start, historical_end, historical_fragment):
                    errors.append(location + ": historical exact fragment differs at its declared line range")
            elif relation == "SYMBOL_SUBSTRING_WITHIN_EXACT_SOURCE_RANGE":
                if not historical_fragment or historical_fragment not in current_fragment:
                    errors.append(location + ": symbol-substring relation contradicts historical fragment")
                if (type(historical_start) is not int or type(historical_end) is not int
                        or historical_start < anchor.get("line", 0)
                        or historical_end > anchor.get("end_line", 0)):
                    errors.append(location + ": symbol-substring relation is outside the current source range")
                if not source_fragment_within_range(path, historical_start, historical_end, historical_fragment):
                    errors.append(location + ": historical symbol is absent from its declared source line range")
            else:
                errors.append(location + ": whole-file match has an unknown historical fragment relation")
        else:
            fragment = anchor.get("exactfragment", "").splitlines()
            lines = path.read_text().splitlines()
            matches = [i + 1 for i in range(len(lines) - len(fragment) + 1)
                       if fragment and lines[i:i + len(fragment)] == fragment]
            if len(matches) != 1 or matches[0] != anchor.get("line"):
                errors.append(location + ": rebound fragment is not unique at its anchor")
            if binding.get("matching_lines_in_current") != matches:
                errors.append(location + ": rebound match inventory is stale")

    for index, node in enumerate(runtime_map.get("new_current_replacement_nodes", [])):
        location = "new_current_replacement_nodes[" + str(index) + "] " + str(node.get("node_id"))
        try:
            path = resolve_map_source(root, node["file"])
            actual = file_digest(path)
        except (KeyError, TypeError, ValueError, OSError):
            errors.append(location + ": current source path is invalid")
            continue
        if node.get("raw_sha256") != actual:
            errors.append(location + ": replacement source hash is stale")
        if not source_fragment_matches(path, node.get("line"), node.get("end_line"), node.get("exactfragment", "")):
            errors.append(location + ": replacement fragment/range is stale")
        if node.get("is_scored_unit") is not False or node.get("runtime_reproducer_result_claimed") is not False:
            errors.append(location + ": replacement node improperly claims acceptance")

    refresh = runtime_map.get("current_source_refresh")
    if not isinstance(refresh, dict):
        errors.append("runtime map lacks a versioned current-source refresh")
    else:
        snapshot_ref = refresh.get("source_snapshot", {})
        try:
            snapshot_path = safe_source(root, snapshot_ref["path"])
            if file_digest(snapshot_path) != snapshot_ref.get("sha256"):
                errors.append("runtime current-source snapshot document hash is stale")
            snapshot = json.loads(snapshot_path.read_text())
            if snapshot.get("iteration") != refresh.get("iteration"):
                errors.append("runtime current-source snapshot iteration differs")
            for entry in snapshot.get("files", []):
                path = safe_source(root, entry["path"])
                if file_digest(path) != entry.get("sha256"):
                    errors.append("runtime current-source snapshot file drift: " + entry["path"])
        except (KeyError, TypeError, ValueError, OSError, json.JSONDecodeError):
            errors.append("runtime current-source snapshot is invalid")
    return errors


def validate_p0_qmi_review(review, root):
    """Fail closed when the finite QMI owner/writer/exit trace loses source facts."""
    errors = []
    label = "P0 QMI static review"
    required_anchors = {
        "main_socket_match", "main_socket_auth", "main_socket_dispatch", "main_public_private_reject",
        "main_retry_producer",
        "main_qmi_shutdown", "http_open_close_format", "owner_mutex_state",
        "private_open_selector", "private_close_fence", "configured_open_writer",
        "macos_factory", "platform_session_close", "late_handoff",
        "retry_worker", "retry_entry", "retry_reconnect", "retry_shutdown_join",
        "selector_auto_exact", "usb_claim", "usb_alternate_failure", "usb_failed_setup_cleanup",
        "usb_reader_construction_cleanup",
        "usb_ctl_sync", "qmi_wire_writer",
        "usb_bulk_writer", "core_close_join", "usb_release",
        "reopen_cleanup_fence", "platform_abort_result",
    } | set(P0_QMI_TYPED_ANCHORS)
    required_instances = set(P0_QMI_BRANCHES)
    required_files = {
        "rust/crates/proletariusd/src/main.rs",
        "rust/crates/proletariusd/src/qmi_enhancement.rs",
        "rust/crates/proletariusd/src/qmi_native_startup.rs",
        "rust/crates/proletarius-qmi-libusb/src/selection.rs",
        "rust/crates/proletarius-qmi-libusb/src/macos.rs",
        "rust/crates/proletarius-qmi-core/src/lib.rs",
        "rust/crates/proletarius-qmi-services/src/client.rs",
        "rust/crates/proletarius-qmi-services/src/uim.rs",
        "app/Sources/BackendProcess.swift",
    }
    if review.get("schema") != "switch.p0.qmi-static-trace.v2" or review.get("iteration") != P0_QMI_CURRENT["iteration"]:
        errors.append(label + ": invalid review identity")
    if (review.get("review_status") != "FINITE_STATIC_OWNER_FORMAT_EXIT_TRACE_REVIEWED_NOT_COMPLETE_P0_GRAPH"
            or review.get("overall_denominator_frozen") is not False
            or review.get("functional_acceptance_awarded") is not False
            or review.get("overall_product_complete") is not False):
        errors.append(label + ": review claims unsupported acceptance")
    if set(review.get("ledger_leaf_ids", [])) != {"P0-2.owner-map", "P2B-4.qmi-control"}:
        errors.append(label + ": leaf scope drift")
    if review.get("excluded_leaf_ids") != ["P2B-4.qmi-uim"]:
        errors.append(label + ": excluded UIM leaf scope drift")
    if set(review.get("api_entry_ids", [])) != {
            "POST /api/internal/qmi/open", "POST /api/internal/qmi/close",
            "POST /api/internal/qmi/probe", "POST /api/internal/qmi/set-ims",
            "POST /api/internal/qmi/operator"}:
        errors.append(label + ": API scope drift")
    if review.get("swift_boundary") != {
            "launch_anchor_id": "swift_default_at_launch",
            "direct_qmi_rpc_calls": [],
            "status": "DEFAULT_AT_NO_DIRECT_SWIFT_QMI_RPC_INTERNAL_CONSUMERS_UNRESOLVED",
            "acceptance_pass_awarded": False}:
        errors.append(label + ": Swift producer boundary drift")
    swift_source = root / "app/Sources"
    if any("/api/internal/qmi/" in path.read_text()
           for path in swift_source.rglob("*.swift") if path.is_file()):
        errors.append(label + ": direct Swift QMI RPC requires a new consumer review")
    if review.get("route_auth_pairs") != P0_QMI_ROUTE_AUTH_PAIRS:
        errors.append(label + ": route authentication pairing drift")
    historical = review.get("historical_runtime_map", {})
    if (historical.get("path") != "docs/optimization/all-rust/runtime-owner-format-exit-map-iteration-645.json"
            or historical.get("family_id") != "device::optional_QMI_start_and_session_finalizer"):
        errors.append(label + ": historical family identity drift")
    else:
        path = safe_source(root, historical["path"])
        if file_digest(path) != historical.get("sha256"):
            errors.append(label + ": historical map pin drift")
        else:
            archived = json.loads(path.read_text())
            if not any(f.get("family_id") == historical["family_id"]
                       and f.get("functional_acceptance_awarded") is False
                       for f in archived.get("families", [])):
                errors.append(label + ": historical family missing or claims acceptance")
    previous = review.get("historical_review", {})
    try:
        previous_path = safe_source(root, P0_QMI_CURRENT["historical_review"])
        previous_review = json.loads(previous_path.read_text())
        if (previous.get("path") != P0_QMI_CURRENT["historical_review"]
                or previous.get("sha256") != file_digest(previous_path)
                or previous_review.get("iteration") != P0_QMI_CURRENT["historical_iteration"]
                or previous_review.get("current_source_snapshot", {}).get("path")
                != P0_QMI_CURRENT["historical_source_snapshot"]
                or file_digest(safe_source(root, previous_review["current_source_snapshot"]["path"]))
                != previous_review["current_source_snapshot"]["sha256"]):
            errors.append(label + ": historical review identity or pin drift")
    except (KeyError, TypeError, ValueError, OSError, json.JSONDecodeError):
        errors.append(label + ": historical review is invalid")

    snapshot_ref = review.get("current_source_snapshot", {})
    snapshot_files = {}
    if snapshot_ref.get("path") != P0_QMI_CURRENT["source_snapshot"]:
        errors.append(label + ": current source snapshot identity drift")
    else:
        snapshot_path = safe_source(root, snapshot_ref["path"])
        if file_digest(snapshot_path) != snapshot_ref.get("sha256"):
            errors.append(label + ": current source snapshot pin drift")
        else:
            try:
                snapshot = json.loads(snapshot_path.read_text())
                snapshot_files = {item["path"]: item["sha256"] for item in snapshot["files"]}
            except (KeyError, TypeError, ValueError, json.JSONDecodeError):
                errors.append(label + ": current source snapshot manifest invalid")

    source_files = review.get("source_files", {})
    if not isinstance(source_files, dict) or set(source_files) != required_files:
        errors.append(label + ": source file set drift")
        source_files = {}
    for relative, expected in source_files.items():
        try:
            if file_digest(safe_source(root, relative)) != expected:
                errors.append(label + ": source file drift: " + relative)
        except (TypeError, ValueError, OSError):
            errors.append(label + ": invalid source file: " + str(relative))
    covered = required_files & set(snapshot_files)
    independently_pinned = required_files - covered
    if (independently_pinned != P0_QMI_INDEPENDENT_PINS
            or any(snapshot_files.get(path) != source_files.get(path) for path in covered)
            or review.get("source_pin_coverage") != {
                "snapshot_source_files": sorted(covered),
                "independent_sha256_source_files": sorted(independently_pinned),
            }):
        errors.append(label + ": dual source pin coverage drift")
    refresh = review.get("source_refresh")
    if (not isinstance(refresh, dict)
            or refresh.get("iteration") != P0_QMI_CURRENT["iteration"]
            or refresh.get("review") != P0_QMI_SOURCE_REFRESH_REVIEW
            or refresh.get("source_files_changed") != sorted(P0_QMI_CHANGED_SOURCE_FILES)
            or refresh.get("unchanged_source_files") != sorted(required_files - P0_QMI_NEW_SOURCE_FILES - P0_QMI_CHANGED_SOURCE_FILES)
            or refresh.get("newly_reviewed_source_files") != sorted(P0_QMI_NEW_SOURCE_FILES)
            or refresh.get("added_anchors") != sorted(P0_QMI_CURRENT_ADDED_ANCHORS)
            or refresh.get("functional_acceptance_awarded") is not False):
        errors.append(label + ": source refresh scope or acceptance drift")

    anchors = review.get("anchors", [])
    anchor_ids = [anchor.get("id") for anchor in anchors] if isinstance(anchors, list) else []
    if len(anchor_ids) != len(set(anchor_ids)) or set(anchor_ids) != required_anchors:
        errors.append(label + ": trace anchor set drift")
    for anchor in anchors if isinstance(anchors, list) else []:
        name, relative = anchor.get("id"), anchor.get("file")
        start, end, fragment = anchor.get("line"), anchor.get("end_line"), anchor.get("exact_fragment")
        if name in P0_QMI_ROUTE_ANCHORS:
            route_start, route_end, required_token = P0_QMI_ROUTE_ANCHORS[name]
            if (relative != "rust/crates/proletariusd/src/main.rs"
                    or (start, end) != (route_start, route_end)
                    or not isinstance(fragment, str) or required_token not in fragment):
                errors.append(label + ": route anchor semantic drift: " + name)
        if name in P0_QMI_TYPED_ANCHORS:
            expected_file, expected_start, expected_end, required_token = P0_QMI_TYPED_ANCHORS[name]
            if ((relative, start, end) != (expected_file, expected_start, expected_end)
                    or not isinstance(fragment, str) or required_token not in fragment):
                errors.append(label + ": typed anchor semantic drift: " + name)
        if relative not in source_files or type(start) is not int or type(end) is not int or start < 1 or end < start:
            errors.append(label + ": invalid trace anchor: " + str(name))
            continue
        lines = safe_source(root, relative).read_text().splitlines(keepends=True)
        actual = "".join(lines[start - 1:end])
        if not isinstance(fragment, str) or not fragment or fragment != actual:
            errors.append(label + ": trace anchor drift: " + str(name))
            continue
        needle = fragment.splitlines()
        source_lines = [line.rstrip("\r\n") for line in lines]
        matches = [i + 1 for i in range(len(source_lines) - len(needle) + 1)
                   if source_lines[i:i + len(needle)] == needle]
        if matches != [start]:
            errors.append(label + ": trace anchor is not unique: " + str(name))

    dependency = review.get("receive_policy_dependency", {})
    expected_dependency = P0_QMI_RECEIVE_POLICY_DEPENDENCY
    if dependency != expected_dependency:
        errors.append(label + ": receive policy dependency declaration drift")
    try:
        dependency_path = safe_source(root, expected_dependency["file"])
        lines = dependency_path.read_text().splitlines(keepends=True)
        if (file_digest(dependency_path) != expected_dependency["sha256"]
                or snapshot_files.get(expected_dependency["file"]) != expected_dependency["sha256"]
                or "".join(lines[expected_dependency["line"] - 1:expected_dependency["end_line"]])
                != expected_dependency["exact_fragment"]):
            errors.append(label + ": receive policy dependency source drift")
    except (KeyError, TypeError, ValueError, OSError):
        errors.append(label + ": receive policy dependency source invalid")

    instances = review.get("candidate_semantic_instances", [])
    instance_ids = [instance.get("id") for instance in instances] if isinstance(instances, list) else []
    if len(instance_ids) != len(set(instance_ids)) or set(instance_ids) != required_instances:
        errors.append(label + ": candidate semantic instance set drift")
    for instance in instances if isinstance(instances, list) else []:
        name = instance.get("id")
        expected = P0_QMI_BRANCHES.get(name)
        if (instance.get("registration") != "CANDIDATE_SOFTWARE_BRANCH_NOT_FROZEN_PRODUCT_INSTANCE"
                or instance.get("hardware_verified") is not False
                or instance.get("acceptance_pass_awarded") is not False
                or expected is None
                or instance.get("trigger") != expected[0]
                or instance.get("static_expected_path") != expected[1]
                or not isinstance(instance.get("anchor_ids"), list)
                or len(instance["anchor_ids"]) != len(set(instance["anchor_ids"]))
                or set(instance["anchor_ids"]) != expected[2]):
            errors.append(label + ": candidate instance scope or acceptance drift: " + str(name))
    if review.get("producer_writer_exit") != P0_QMI_ROLES:
        errors.append(label + ": producer/format/writer/exit semantic drift")
    operations = review.get("typed_operations", [])
    operation_ids = [operation.get("id") for operation in operations] if isinstance(operations, list) else []
    if len(operation_ids) != len(set(operation_ids)) or set(operation_ids) != set(P0_QMI_TYPED_OPERATIONS):
        errors.append(label + ": typed operation set drift")
    for operation in operations if isinstance(operations, list) else []:
        name = operation.get("id")
        expected = P0_QMI_TYPED_OPERATIONS.get(name)
        if (expected is None
                or any(operation.get(field) != value for field, value in expected.items()
                       if field != "anchor_ids")
                or not isinstance(operation.get("anchor_ids"), list)
                or len(operation["anchor_ids"]) != len(set(operation["anchor_ids"]))
                or set(operation["anchor_ids"]) != expected["anchor_ids"]
                or operation.get("registration") != "STATIC_SOFTWARE_TRACE_NOT_FROZEN_INSTANCE"
                or operation.get("hardware_verified") is not False
                or operation.get("acceptance_pass_awarded") is not False):
            errors.append(label + ": typed operation scope or acceptance drift: " + str(name))
    if review.get("unresolved") != P0_QMI_UNRESOLVED:
        errors.append(label + ": unresolved boundary drift")
    return errors


def validate_source_successor_conservation(ledger, runtime_map, root):
    """Retain historical facts and event occurrences through this finite successor."""
    errors = []
    try:
        path = safe_source(root, SOURCE_SUCCESSOR_CONSERVATION["path"])
        if file_digest(path) != SOURCE_SUCCESSOR_CONSERVATION["sha256"]:
            return ["source successor: conservation document pin drift"]
        document = json.loads(path.read_text())
    except (KeyError, TypeError, ValueError, OSError, json.JSONDecodeError):
        return ["source successor: conservation document invalid"]
    family_keys = ("family_id", "existing_leaf_ids", "leaf_declared_scenario_context_ids",
                   "historical_source_node_ids", "original_source_graph_family",
                   "new_scored_instance", "functional_acceptance_awarded")
    current = {
        "parents": ledger.get("parents"), "leaves": ledger.get("leaves"),
        "classes": ledger.get("applicability_matrix"),
        "policies": ledger.get("unresolved_policies"),
        "historical_node_identity_records": [
            {"node_id": n.get("node_id"), "historical": n.get("historical")}
            for n in runtime_map.get("historical_source_nodes", [])],
        "root_adoption": runtime_map.get("root_adoption"),
        "runtime_family_identities": [
            {key: n.get(key) for key in family_keys}
            for n in runtime_map.get("families", [])],
    }
    try:
        current["scope_protocol"] = json.loads(safe_source(root, str(BASE / "acceptance-scope-protocol.json")).read_text())
        current["baseline_provenance"] = json.loads(safe_source(root, str(BASE / "baseline-provenance.json")).read_text())
    except (ValueError, OSError, json.JSONDecodeError):
        errors.append("source successor: normative document invalid")
    expected = document.get("conserved_payload_sha256", {})
    if expected != SOURCE_SUCCESSOR_CONSERVATION["conserved_payload_sha256"]:
        errors.append("source successor: conserved payload declaration drift")
    for key, digest in SOURCE_SUCCESSOR_CONSERVATION["conserved_payload_sha256"].items():
        if canonical_digest(current.get(key)) != digest:
            errors.append("source successor: immutable payload drift: " + key)
    events = document.get("original_754_events", [])
    if (len(events) != 912 or len({e.get("event_id") for e in events}) != 912
            or [e.get("error_index") for e in events] != list(range(912))
            or len({e.get("error") for e in events}) != 909
            or canonical_digest(events) != SOURCE_SUCCESSOR_CONSERVATION["original_events_sha256"]
            or any(e.get("event_id") != digest_bytes((str(e.get("error_index")) + "\0" + str(e.get("error"))).encode()) for e in events)):
        errors.append("source successor: historical event occurrence drift")
    dispositions = document.get("event_dispositions", [])
    permitted = {"CURRENT_EXACT_SOURCE_BINDING", "EXPLICIT_SEMANTIC_SOURCE_SUCCESSOR",
                 "EXPLICIT_WIDER_SWIFT_OWNER_SUCCESSOR", "VERSIONED_QMI_RECEIVE_POLICY_SUCCESSOR",
                 "VERSIONED_CURRENT_FILE_PIN_SUCCESSOR"}
    if (len(dispositions) != 912 or {x.get("event_id") for x in dispositions} != {e.get("event_id") for e in events}
            or len({x.get("event_id") for x in dispositions}) != 912
            or any(x.get("disposition") not in permitted or not x.get("binding_reference")
                   or x.get("functional_acceptance_awarded") is not False for x in dispositions)):
        errors.append("source successor: event disposition closure drift")
    historical = {n.get("node_id"): n for n in runtime_map.get("historical_source_nodes", [])}
    active = {n.get("node_id"): n for n in runtime_map.get("new_current_replacement_nodes", [])}
    superseded = {n.get("node_id"): n for n in runtime_map.get("superseded_current_replacement_nodes", [])}
    for row in document.get("semantic_successors", []):
        if row.get("kind") == "HISTORICAL":
            node = historical.get(row.get("node_id"), {})
            binding = node.get("current_binding", {})
            if (binding.get("status") != "SUPERSEDED_OR_REANCHOR_REQUIRED"
                    or binding.get("current_anchor") is not None
                    or binding.get("final_source_snapshot_bound") is not False
                    or row.get("prior_binding_sha256") not in
                    {canonical_digest(b) for b in node.get("current_binding_history", [])}):
                errors.append("source successor: historical supersession drift: " + str(row.get("node_id")))
        else:
            prior = superseded.get(row.get("node_id"), {})
            if (row.get("node_id") in active
                    or canonical_digest(prior.get("superseded_prior_object")) != row.get("prior_object_sha256")
                    or prior.get("final_source_snapshot_bound") is not False):
                errors.append("source successor: replacement supersession drift: " + str(row.get("node_id")))
        for node_id in row.get("successor_node_ids", []):
            node = active.get(node_id, {})
            if (node.get("predecessor_node_id") != row.get("node_id")
                    or node.get("is_scored_unit") is not False
                    or node.get("runtime_reproducer_result_claimed") is not False
                    or node.get("acceptance_awarded") is not False
                    or node.get("final_source_snapshot_bound") is not True
                    or node.get("source_snapshot_sha256") != runtime_map.get("current_source_refresh", {}).get("source_snapshot", {}).get("sha256")):
                errors.append("source successor: independent current node drift: " + str(node_id))
    return errors


def check(root):
    bundle = {}
    pins = {}
    for key, name in FILES.items():
        path = safe_source(root, str(BASE / name))
        pins[str(BASE / name)] = file_digest(path)
        bundle[key] = json.loads(path.read_text())
    rt = safe_source(root, str(BASE / "runtime-capability-inventory.md"))
    pins[str(rt.relative_to(root.resolve()))] = file_digest(rt)
    result = validate_bundle(bundle, runtime_rows(rt.read_text()), root)
    result["errors"].extend(validate_source_pins(bundle["mapping"], root))
    runtime_path = safe_source(root, RUNTIME_MAP)
    runtime_map = json.loads(runtime_path.read_text())
    result["errors"].extend(validate_runtime_source_map(runtime_map, root))
    result["errors"].extend(validate_source_successor_conservation(bundle["ledger"], runtime_map, root))
    pins[RUNTIME_MAP] = file_digest(runtime_path)
    snapshot_ref = runtime_map.get("current_source_refresh", {}).get("source_snapshot", {})
    if snapshot_ref.get("path"):
        try:
            pins[snapshot_ref["path"]] = file_digest(safe_source(root, snapshot_ref["path"]))
        except (KeyError, TypeError, ValueError, OSError):
            pins[snapshot_ref["path"]] = None
    result["errors"].extend(validate_adopted_document_pins(
        bundle["ledger"].get("source_catalog", {}), pins, snapshot_ref))
    try:
        current_snapshot = json.loads(safe_source(root, CURRENT_SOURCE_SNAPSHOT["path"]).read_text())
        result["errors"].extend(validate_immutable_source_ancestry(current_snapshot, root))
        previous_path = safe_source(root, CURRENT_SOURCE_SNAPSHOT["previous_path"])
        if file_digest(previous_path) != CURRENT_SOURCE_SNAPSHOT["previous_sha256"]:
            result["errors"].append("current source refresh: immutable predecessor snapshot pin drift")
        else:
            previous_snapshot = json.loads(previous_path.read_text())
            result["errors"].extend(validate_current_source_refresh(
                current_snapshot, previous_snapshot, bundle["mapping"], runtime_map))
    except (KeyError, TypeError, ValueError, OSError, json.JSONDecodeError):
        result["errors"].append("current source refresh: source snapshot is invalid")
    qmi_review_path = safe_source(root, P0_QMI_REVIEW)
    qmi_review = json.loads(qmi_review_path.read_text())
    result["errors"].extend(validate_p0_qmi_review(qmi_review, root))
    pins[P0_QMI_REVIEW] = file_digest(qmi_review_path)
    source_identity_script = safe_source(root, "scripts/source-identity.py")
    current_source_digest = subprocess.check_output(
        [sys.executable, str(source_identity_script)], cwd=root, text=True
    ).strip()
    if re.fullmatch(r"[0-9a-f]{64}", current_source_digest) is None:
        raise ValueError("current source digest is malformed")
    result["source_digest"] = current_source_digest
    result["document_sha256"] = pins
    result["metadata_consistent"] = not result["errors"]
    return result


def validate_adopted_document_pins(catalog, observed_pins, snapshot_ref):
    errors = []
    for key, relative in (("scope_protocol", str(BASE / FILES["protocol"])),
                          ("baseline_provenance", str(BASE / FILES["baselines"])),
                          ("entry_mapping", str(BASE / FILES["mapping"])),
                          ("api-contract_inventory", str(BASE / FILES["inventory"]))):
        declaration = catalog.get(key, {})
        if declaration.get("path") != "src/" + relative or declaration.get("sha256") != observed_pins.get(relative):
            errors.append("adopted document pin drift: " + key)
    runtime_decl = catalog.get("runtime_owner_format_exit_mapping", {})
    if (runtime_decl.get("path") != "src/" + RUNTIME_MAP
            or runtime_decl.get("sha256") != observed_pins.get(RUNTIME_MAP)):
        errors.append("adopted document pin drift: runtime_owner_format_exit_mapping")
    snapshot_path = snapshot_ref.get("path") if isinstance(snapshot_ref, dict) else None
    snapshot_digest = snapshot_ref.get("sha256") if isinstance(snapshot_ref, dict) else None
    snapshot_decl = catalog.get("current_source_snapshot", {})
    if (not snapshot_path or snapshot_decl.get("path") != "src/" + snapshot_path
            or snapshot_decl.get("sha256") != snapshot_digest
            or observed_pins.get(snapshot_path) != snapshot_digest):
        errors.append("adopted document pin drift: current_source_snapshot")
    return errors


def validate_immutable_source_ancestry(snapshot, root):
    """Verify all adopted predecessor identities without re-testing old source."""
    errors = []
    expected = CURRENT_SOURCE_SNAPSHOT["immutable_ancestry"]
    if snapshot.get("immutable_ancestry") != expected:
        errors.append("current source refresh: immutable ancestry declaration drift")
    for item in expected:
        try:
            path = safe_source(root, item["path"])
            value = json.loads(path.read_text())
            if (file_digest(path) != item["sha256"]
                    or value.get("iteration") != item["iteration"]
                    or value.get("previous_snapshot") != item["previous_snapshot"]):
                errors.append("current source refresh: immutable ancestry pin drift: " + item["path"])
        except (KeyError, TypeError, ValueError, OSError, json.JSONDecodeError):
            errors.append("current source refresh: immutable ancestry invalid: " + item["path"])
    if snapshot.get("immutable_documents") != CURRENT_SOURCE_SNAPSHOT["immutable_documents"]:
        errors.append("current source refresh: immutable document declaration drift")
    for relative, digest in CURRENT_SOURCE_SNAPSHOT["immutable_documents"].items():
        try:
            if file_digest(safe_source(root, relative)) != digest:
                errors.append("current source refresh: immutable document pin drift: " + relative)
        except (ValueError, OSError):
            errors.append("current source refresh: immutable document invalid: " + relative)
    return errors


def validate_current_source_refresh(snapshot, previous, mapping, runtime_map):
    """Constrain the active selective refresh to its explicitly reviewed source set."""
    errors = []
    expected = CURRENT_SOURCE_SNAPSHOT
    if (snapshot.get("schema") != "switch.current-source-snapshot.v1"
            or snapshot.get("iteration") != expected["iteration"]
            or snapshot.get("previous_snapshot") != {
                "path": expected["previous_path"], "sha256": expected["previous_sha256"]
            }):
        errors.append("current source refresh: snapshot identity or predecessor drift")
    old_rows, new_rows = previous.get("files", []), snapshot.get("files", [])
    old = {row.get("path"): row.get("sha256") for row in old_rows}
    new = {row.get("path"): row.get("sha256") for row in new_rows}
    if (len(old_rows) != expected["previous_file_count"]
            or len(new_rows) != expected["file_count"]
            or len(old) != expected["previous_file_count"] or len(new) != expected["file_count"]
            or list(new) != sorted(new)):
        errors.append("current source refresh: selective source inventory changed unexpectedly")
    if (set(old) - set(new) or set(new) - set(old) != expected["added_files"]
            or {path for path in set(old) & set(new) if old[path] != new[path]}
            != expected["changed_files"]):
        errors.append("current source refresh: reviewed changed/added file set drift")
    if (snapshot.get("scope_metadata_binding", {}).get("new_leaf_ids") != 0
            or snapshot.get("scope_metadata_binding", {}).get("new_functional_passes") != 0):
        errors.append("current source refresh: snapshot claims unsupported acceptance")
    if snapshot.get("software_acceptance_scope") != SOFTWARE_ONLY_SCOPE:
        errors.append("current source refresh: software-only scope drift")
    for label, owner in (("entry", mapping), ("runtime", runtime_map)):
        refresh = owner.get("current_source_refresh", {})
        changed_rows = refresh.get("changed_source_files", [])
        added_rows = refresh.get("snapshot_added_source_files", [])
        changed = {row.get("file") for row in changed_rows}
        added = {row.get("file") for row in added_rows}
        snapshot_ref = refresh.get("source_snapshot")
        snapshot_path = (snapshot_ref.get("path") if isinstance(snapshot_ref, dict)
                         else snapshot_ref)
        if (refresh.get("iteration") != expected["iteration"] or changed != expected["changed_files"]
                or added != expected["added_files"]
                or len(changed_rows) != len(expected["changed_files"])
                or len(added_rows) != len(expected["added_files"])
                or snapshot_path != expected["path"]
                or any(row.get("previous_sha256") != old.get(row.get("file"))
                       or row.get("current_sha256") != new.get(row.get("file"))
                       for row in changed_rows)
                or any(row.get("sha256") != new.get(row.get("file")) for row in added_rows)
                or refresh.get("functional_acceptance_awarded") is not False
                or refresh.get("software_acceptance_scope") != SOFTWARE_ONLY_SCOPE):
            errors.append("current source refresh: " + label + " scope or acceptance drift")
    return errors


def acceptance_gate(result, *, require_frozen=False, require_product_complete=False):
    """Return whether the requested ledger gate is explicitly satisfied.

    Product completion is intentionally a separate, stronger predicate.  This
    prevents a future frozen denominator from silently promoting a release
    while functional acceptance or the product-complete decision is absent.
    """
    if not result.get("metadata_consistent"):
        return False
    if require_product_complete:
        return (result.get("overall_denominator_frozen") is True
                and result.get("functional_acceptance_awarded") is True
                and result.get("overall_product_complete") is True)
    if require_frozen:
        return result.get("overall_denominator_frozen") is True
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--report", type=Path, help="create-only JSON report; existing evidence is never overwritten")
    parser.add_argument("--require-frozen", action="store_true", help="fail while the overall denominator remains unresolved")
    parser.add_argument(
        "--require-product-complete", action="store_true",
        help=("fail unless the ledger explicitly records a frozen denominator, "
              "awarded functional acceptance, and overall product completion"),
    )
    args = parser.parse_args()
    try:
        result = check(args.root.resolve())
        result["require_frozen"] = args.require_frozen
        result["require_product_complete"] = args.require_product_complete
        result["gate_satisfied"] = acceptance_gate(
            result,
            require_frozen=args.require_frozen,
            require_product_complete=args.require_product_complete,
        )
        if args.report:
            with args.report.open("x") as stream:
                json.dump(result, stream, ensure_ascii=False, indent=2)
                stream.write("\n")
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0 if result["gate_satisfied"] else 1
    except (KeyError, TypeError, ValueError, OSError) as error:
        print("FAIL: " + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
