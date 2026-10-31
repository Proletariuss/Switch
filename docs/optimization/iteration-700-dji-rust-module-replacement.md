# DJI-Rust-Modulersatz und VoWiFi-Anbindung

Status: Migrationszweig, noch kein Hardware- oder Betreiberfreigabestand.

## Ziel

Die bisherige Rust-Modulsteuerung wird durch eine neue Rust-Adaptergrenze für das DJI-Modul ersetzt. Der Adapter übernimmt USB-Erkennung, USB-AT, QMI/MBIM-Erweiterungen, ADB-Schnittstelle, SIM/eSIM, SMS, Netzmodus und das QDC507-Sprachmanagement. Der Go-Backend-Code aus DJOneHubNative wird nicht eingebunden.

Die bestehende Rust-VoWiFi-Schicht bleibt die einzige IMS-/IKE-/ESP-/SIP-Implementierung. Der Moduladapter stellt dieselben Karten- und AKA-Fähigkeiten bereit wie der Reader-Adapter. Dadurch benutzen Reader und DJI-Modul dieselbe ePDG-/IMS-/Codec-Pipeline, während Transport und physischer Besitz getrennt bleiben.

## Quellen und Lizenz

Als Verhaltensreferenz dient DJOneHubNative, Revision 7194ae8cd71da34e0ab3727874dafb82ffd2537b:

<https://github.com/cr-zhichen/DJOneHubNative/tree/7194ae8cd71da34e0ab3727874dafb82ffd2537b>

Die Quelle steht unter PolyForm Noncommercial 1.0.0. Die erforderlichen Hinweise, die Lizenzdatei und die Herkunft von abgeleiteten Modulverhaltensweisen bleiben im Repository erhalten. Die Rust-Neuimplementierung wird nur im Rahmen der zulässigen nichtkommerziellen Nutzung verteilt.

## Zielgrenzen

- Rust besitzt Modultransport, Besitz, Session-Generationen, AT/QMI/MBIM, ADB, SMS, eSIM und Netzwerkstatus.
- Rust besitzt weiterhin IMS, IKE/ESP, SIP, EAP-AKA, Proxy/Fake-IP-Vertrag und EVS/AMR-WB-Aushandlung.
- Swift besitzt UI, CoreBluetooth und Audio-Geräteauswahl.
- Reader und DJI-Modul verwenden getrennte physische Besitzer, aber dieselbe VoWiFi-Session- und Codec-Policy.
- Alte Restore-/Lock-Dateien werden weder automatisch gelöscht noch als erledigt markiert. Die neue Adaptergrenze darf eine sichere Hardwareprüfung nicht durch eine historische Statusdatei ersetzen.

## Migrationsreihenfolge

1. Adaptervertrag und Capability-Projektion definieren.
2. DJI-USB-AT- und QMI/MBIM-Erkennung mit exakter Geräte-/Interface-Bindung portieren.
3. ADB claim/close, QDC507-Sprachvorbereitung und Netzwerk-/SMS-/eSIM-Dienste portieren.
4. ModuleSessionCard auf den gemeinsamen Karten-/AKA-Vertrag umstellen.
5. Modul-VoWiFi über den bestehenden Reader-VoWiFi-Stack ausführen.
6. Alte Modul-Transportpfade aus dem Build entfernen, sobald der neue Adapter alle Routen übernimmt.
7. Rust-/Swift-Gates, Lizenzprüfung, Bundle-Audit und Hardware-Akzeptanz getrennt durchführen.

## Freigabesperren

Ein Build darf erst als Modul-VoWiFi-fähig bezeichnet werden, wenn USB-AT, SIM-Identität, AKA, ePDG/IMS-Registrierung, eingehender und ausgehender Ruf, bidirektionales Audio, SMS und kontrollierte Freigabe jeweils nachgewiesen sind. Ohne DJI-Hardware bleibt die Prüfung softwareseitig und darf keine erfolgreiche Betreiber- oder Hardwarefreigabe behaupten.
