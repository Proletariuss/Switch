#!/usr/bin/env python3
"""Opt-in, isolated reader VoWiFi registration probe with redacted output.

This probe never switches/downloads profiles, sends SMS, or places calls.
It only starts after the selected reader reports the expected operator.
"""

import argparse
import http.client
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import sys
import time
import uuid


class UnixHTTP(http.client.HTTPConnection):
    def __init__(self, path, timeout):
        super().__init__("localhost", timeout=timeout)
        self.path = path

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.path)


def request(path, method="GET", body=None, timeout=12):
    connection = UnixHTTP(SOCKET_PATH, timeout)
    payload = None if body is None else json.dumps(body).encode()
    connection.request(method, path, body=payload, headers={"Content-Type": "application/json"})
    response = connection.getresponse()
    data = response.read(1024 * 1024)
    connection.close()
    try:
        value = json.loads(data) if data else {}
    except json.JSONDecodeError:
        value = {}
    return response.status, value


def diagnostic(status):
    candidate = status.get("startup_failure") or status.get("diagnostic") or {}
    if not isinstance(candidate, dict):
        candidate = {}
    allowed = (
        "stage", "code", "io_operation", "io_kind", "esp_sent", "esp_received",
        "esp_rejected", "ike_received_after_child", "keepalive_received_after_child",
        "invalid_datagram_after_child",
    )
    return {key: candidate[key] for key in allowed if key in candidate}


def status_summary(value):
    return {
        key: value.get(key)
        for key in ("tunnel_state", "ims_state", "sms_ready", "call_ready")
        if key in value
    } | {"diagnostic": diagnostic(value)}


def profile_overview_summary(http_status, value):
    """Expose only read health and counts, never EID, ICCID, or profile names."""
    if http_status != 200 or not isinstance(value, dict):
        return {"http_status": http_status, "complete": False, "eid_count": 0,
                "profile_group_count": 0, "profile_count": 0}
    chip_info = value.get("chip_info") if isinstance(value.get("chip_info"), dict) else {}
    eids = chip_info.get("eids") if isinstance(chip_info.get("eids"), list) else []
    groups = value.get("profiles") if isinstance(value.get("profiles"), list) else []
    profile_count = sum(len(group.get("profiles", [])) for group in groups
                        if isinstance(group, dict) and isinstance(group.get("profiles"), list))
    complete = bool(eids) and len(eids) == len(groups) and all(
        isinstance(group, dict) and group.get("read_state") == "ready" for group in groups
    )
    return {"http_status": http_status, "complete": complete, "eid_count": len(eids),
            "profile_group_count": len(groups), "profile_count": profile_count}


def safe_error_class(value):
    message = value.get("error", "") if isinstance(value, dict) else ""
    if not isinstance(message, str):
        return "unknown"
    for marker in (
        "native_lifecycle_not_accepting", "native_lifecycle_unavailable",
        "reader_identity_unavailable", "reader_identity_incomplete",
        "reader_isim_identity_incomplete", "reader_operation_active",
        "reader_native_lease_active", "VoWiFi 会话已在运行", "运营商配置",
        "代理", "IKE", "EAP", "IMS", "timed out", "timeout",
    ):
        if marker in message:
            return marker
    # Preserve only Chinese diagnostic words and punctuation. Addresses,
    # subscriber identifiers, paths, and internal tokens are always removed.
    hint = re.sub(r"[A-Za-z0-9_./:+-]+", "[redacted]", message)
    hint = re.sub(r"\[redacted\](?:\s*\[redacted\])+", "[redacted]", hint)
    return hint[:100] if hint else "unclassified"


def isolated_runtime_directory(runtime_root, suffix):
    """Return an absolute per-probe data directory for the native daemon."""
    return Path(runtime_root).expanduser().resolve() / ("reader-vowifi-" + suffix)


def isolated_socket_path(suffix):
    """Keep Darwin's public and companion AF_UNIX paths below SUN_LEN."""
    return Path("/tmp") / ("sw-" + suffix) / "api.sock"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--reader", default="ESTKme-RED")
    parser.add_argument("--operator", default="SIM World")
    parser.add_argument("--runtime-root", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--wait-seconds", type=int, default=105)
    parser.add_argument("--read-profile-overview", action="store_true",
                        help="read EID/Profile overview and report only counts and read state")
    args = parser.parse_args()

    # Fail before creating the isolated runtime when a build was cleaned or
    # the caller selected a directory instead of an executable daemon.
    if not args.binary.is_file() or not os.access(args.binary, os.X_OK):
        parser.error("--binary must name an existing executable daemon")

    suffix = uuid.uuid4().hex[:12]
    root = isolated_runtime_directory(args.runtime_root, suffix)
    root.mkdir(parents=True, mode=0o700)
    socket_root = isolated_socket_path(suffix).parent
    socket_root.mkdir(mode=0o700)
    global SOCKET_PATH
    SOCKET_PATH = str(isolated_socket_path(suffix))
    env = os.environ.copy()
    env["SWITCH_DATA_DIR"] = str(root)
    report = {
        "scope": "isolated selected-reader VoWiFi registration only",
        "reader_found": False,
        "operator_confirmed": False,
        "vowifi_start_attempted": False,
        "vowifi_started": False,
        "vowifi_stop_confirmed": False,
        "reader_disconnect_confirmed": False,
        "daemon_exit_confirmed": False,
        "profile_changed": False,
        "sms_sent": False,
        "call_placed": False,
        "transitions": [],
    }
    try:
        process = subprocess.Popen(
            [str(args.binary), "-listen", "unix:" + SOCKET_PATH],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except OSError:
        shutil.rmtree(root)
        shutil.rmtree(socket_root)
        parser.error("cannot start the selected daemon")
    connected = False
    try:
        for _ in range(100):
            if process.poll() is not None:
                raise RuntimeError("isolated daemon exited during startup")
            if os.path.exists(SOCKET_PATH):
                try:
                    if request("/api/health")[0] == 200:
                        break
                except (OSError, http.client.HTTPException):
                    pass
            time.sleep(0.1)
        else:
            raise RuntimeError("isolated daemon did not become healthy")

        code, readers = request("/api/reader/list")
        available = readers.get("readers", []) if isinstance(readers, dict) else []
        available = [name for name in available if isinstance(name, str)]
        report["available_reader_count"] = len(available)
        matches = [name for name in available if args.reader.casefold() in name.casefold()]
        report["reader_name_match_count"] = len(matches)
        if code != 200 or not matches:
            raise RuntimeError("selected reader unavailable")
        report["reader_found"] = True
        code, info = request("/api/reader/connect", "POST", {"reader": matches[0]}, timeout=60)
        if code != 200:
            raise RuntimeError("selected reader connection failed")
        connected = True
        usim = info.get("usim_info") or {}
        operator = usim.get("operator_name") or usim.get("operatorName") or ""
        canonical = lambda name: "".join(character for character in name.casefold() if character.isalnum())
        report["operator_confirmed"] = canonical(operator) == canonical(args.operator)
        report["mcc_mnc"] = str(usim.get("mcc", "")) + "/" + str(usim.get("mnc", ""))
        if not report["operator_confirmed"]:
            raise RuntimeError("active operator does not match requested test card")

        if args.read_profile_overview:
            code, overview = request("/api/reader/esim")
            summary = profile_overview_summary(code, overview)
            report["profile_overview"] = summary
            if not summary["complete"]:
                raise RuntimeError("selected reader EID/Profile overview was incomplete")

        report["vowifi_start_attempted"] = True
        code, start = request("/api/reader/vowifi", "POST", {"enable": True, "proxy": ""})
        report["start_http_status"] = code
        if isinstance(start, dict) and isinstance(start.get("code"), str):
            report["start_error_code"] = start["code"][:80]
        if code not in (200, 202, 204):
            report["start_error_class"] = safe_error_class(start)
            try:
                status_code, status_value = request("/api/reader/vowifi")
                if status_code == 200:
                    report["final_status"] = status_summary(status_value)
                    if isinstance(status_value.get("carrier_config_error"), str):
                        report["carrier_config_error_code"] = status_value["carrier_config_error"][:80]
            except (OSError, http.client.HTTPException):
                pass
            raise RuntimeError("VoWiFi start request rejected")
        report["vowifi_started"] = True
        previous = None
        deadline = time.monotonic() + args.wait_seconds
        while time.monotonic() < deadline:
            code, value = request("/api/reader/vowifi")
            if code == 200:
                summary = status_summary(value)
                transition = (summary.get("tunnel_state"), summary.get("ims_state"))
                if transition != previous:
                    report["transitions"].append(dict(zip(("tunnel_state", "ims_state"), transition)))
                    previous = transition
                report["final_status"] = summary
                if transition[0] == "error" or transition[1] in ("failed", "registered"):
                    break
            time.sleep(2)
    except (RuntimeError, OSError, http.client.HTTPException, ValueError) as error:
        report["probe_error"] = str(error)
    finally:
        if report["vowifi_start_attempted"]:
            try:
                report["vowifi_stop_confirmed"] = request(
                    "/api/reader/vowifi", "POST", {"enable": False}
                )[0] in (200, 202, 204)
            except (OSError, http.client.HTTPException):
                pass
        if connected:
            try:
                report["reader_disconnect_confirmed"] = request(
                    "/api/reader/disconnect", "POST", {}
                )[0] == 200
            except (OSError, http.client.HTTPException):
                pass
        if process.poll() is None:
            process.send_signal(signal.SIGTERM)
        try:
            report["daemon_exit_confirmed"] = process.wait(timeout=15) == 0
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        if process.poll() is not None:
            shutil.rmtree(root)
            shutil.rmtree(socket_root)
            report["runtime_removed"] = True
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps(report, ensure_ascii=False, indent=2))
    ready = report.get("final_status", {}).get("ims_state") == "registered"
    profiles_ready = not args.read_profile_overview or report.get("profile_overview", {}).get("complete") is True
    clean = (report["vowifi_stop_confirmed"] and report["reader_disconnect_confirmed"]
             and report["daemon_exit_confirmed"])
    return 0 if ready and profiles_ready and clean else 1


if __name__ == "__main__":
    sys.exit(main())
