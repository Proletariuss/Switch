#!/usr/bin/env python3
"""Offline comparison of fixed public references; never changes carrier data.

Run once to emit synthetic probes, replay them with the real Rust carrier
resolver, then run with --replay to emit coverage and conflict observations.
Raw VoCat files stay in the research directory because its root license is
Research & Evaluation. This independent audit does not import its runtime.
"""

import argparse
from collections import Counter
import copy
import hashlib
import itertools
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
WORKSPACE = ROOT.parent
VOCAT_SHA = "0dec0cbfa3445695e44a5108d88db399f639fccb"
MIT_SHA = "1751fd3d3629927ae8499aa9d682130c025a965d"
FIELDS = {"imsi_prefixes": "imsi", "iccid_prefixes": "iccid",
          "gid1_prefixes": "gid1", "gid2_prefixes": "gid2", "spns": "spn"}


def inside_workspace(path):
    path = Path(path).resolve()
    if not path.is_relative_to(WORKSPACE):
        raise ValueError("audit inputs and output must remain inside Switch")
    return path


def read_json(path):
    return json.loads(path.read_text())


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def key(sim):
    return json.dumps(sim, sort_keys=True)


def merge(destination, patch):
    for name, value in patch.items():
        if isinstance(value, dict) and isinstance(destination.get(name), dict):
            merge(destination[name], value)
        else:
            destination[name] = copy.deepcopy(value)


def epdg(sim, configuration):
    settings = configuration.get("ims", {}).get("epdg", {})
    return (settings.get("address")
            or next((entry.get("address") for entry in settings.get("candidates", [])
                     if entry.get("address")), None)
            or settings.get("remote_certificate_hostname")
            or standard_epdg(sim))


def standard_epdg(sim):
    return f"epdg.epc.mnc{sim['mnc'].zfill(3)}.mcc{sim['mcc']}.pub.3gppnetwork.org"


def synthetic_probes(profiles):
    observations = []
    for profile in profiles:
        matches = ([profile["match"]] if profile.get("match") else [])
        matches += profile.get("match_any", [])
        for index, match in enumerate(matches):
            unknown = set(match) - set(FIELDS) - {"home_plmns"}
            if unknown:
                raise ValueError(f"unreviewed reference match fields: {sorted(unknown)}")
            fields = [(source, target) for source, target in FIELDS.items() if match.get(source)]
            choices = [match[source] for source, _ in fields]
            for plmn in match.get("home_plmns", ["99942"]):
                if len(plmn) not in (5, 6) or not plmn.isascii() or not plmn.isdigit():
                    raise ValueError("unreviewed reference PLMN")
                for values in itertools.product(*choices):
                    sim = {"mcc": plmn[:3], "mnc": plmn[3:]}
                    sim.update({target: value for (_, target), value in zip(fields, values)})
                    observations.append({"reference_id": profile["id"], "match_index": index,
                                         "sim": sim, "unscoped": not match.get("home_plmns"),
                                         "reference_epdg": profile.get("epdg", {}).get("hostname")})
                    if len(observations) > 10000:
                        raise ValueError("reference probe budget exceeded")
    return observations


def leaves(value, path=""):
    if isinstance(value, dict):
        for name, child in value.items():
            yield from leaves(child, f"{path}.{name}".strip("."))
    else:
        yield path, value


def replay_counts(replay):
    """Fallback is an accepted standard profile, not a rejected identity."""
    return {
        "total": len(replay),
        "matched": sum(item.get("matched", False) is True for item in replay),
        "fallback": sum(not item.get("matched", False) and not item["rejected"] for item in replay),
        "rejected": sum(item["rejected"] is True for item in replay),
        "runtime_profile_present": sum(bool(item.get("runtime_profile")) for item in replay),
        "runtime_profile_absent": sum(not item.get("runtime_profile") for item in replay),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference", default=WORKSPACE / "build/research/vocat-carriers-20261005")
    parser.add_argument("--output", default=WORKSPACE / "build/research/vocat-carriers-20261005/audit")
    parser.add_argument("--replay", help="Rust carrier_replay result for the emitted cases")
    args = parser.parse_args()
    reference, output = inside_workspace(args.reference), inside_workspace(args.output)
    manifest = read_json(reference / "manifest.json")
    if manifest["sha"] != VOCAT_SHA:
        raise ValueError("reference revision changed; review before accepting new data")
    for item in manifest["files"]:
        path = (reference / item["path"]).resolve()
        if not path.is_relative_to(reference):
            raise ValueError("reference manifest path escaped its root")
        if hashlib.sha256(path.read_bytes()).hexdigest() != item["sha256"]:
            raise ValueError(f"reference hash differs: {item['path']}")
    mit_manifest = read_json(reference / "ios-carrier-profiles-manifest.json")
    if mit_manifest["sha"] != MIT_SHA:
        raise ValueError("MIT source revision changed; review before accepting new data")
    source = reference / "ios-carrier-profiles"
    for item in mit_manifest["files"]:
        path = (source / item["path"]).resolve()
        if not path.is_relative_to(source):
            raise ValueError("MIT manifest path escaped its root")
        if hashlib.sha256(path.read_bytes()).hexdigest() != item["sha256"]:
            raise ValueError(f"MIT source hash differs: {item['path']}")
    # Ruby/Psych is used only to parse the pinned public YAML, not execute it.
    ruby = 'r={}; Dir.glob(File.join(ARGV[0],"profiles","*.yaml")).sort.each{|f|p=YAML.safe_load(File.read(f),aliases: true);r[p["id"]]=p};STDOUT.write(JSON.generate(r))'
    mit_profiles = json.loads(subprocess.check_output(["ruby", "-ryaml", "-rjson", "-e", ruby, str(source)]))
    vocat = read_json(reference / "internal/vowifi/carrier_profiles.json")
    registry = read_json(ROOT / "rust/carrier-registry.json")
    observations = synthetic_probes(vocat["profiles"])
    cases = {key(case["sim"]): case for case in read_json(ROOT / "rust/carrier-cases.json")}
    cases.update({key(item["sim"]): {"sim": item["sim"]} for item in observations})
    cases = list(cases.values())
    output.mkdir(parents=True, exist_ok=True)
    write_json(output / "synthetic-cases.json", cases)
    plmns = {p for profile in vocat["profiles"]
             for match in ([profile["match"]] if profile.get("match") else []) + profile.get("match_any", [])
             for p in match.get("home_plmns", [])}
    fields = Counter(path for profile in vocat["profiles"]
                     for path, _ in leaves({k: v for k, v in profile.items()
                                           if k not in {"id", "match", "match_any"}}))
    summary = {"schema_version": 1, "vocat_revision": VOCAT_SHA, "mit_revision": MIT_SHA,
               "switch_revision": registry["revision"], "hardware_verified": False,
               "comparison_scope": "first effective ePDG only; no DNS, candidate-set equivalence, operator entitlement or reference runtime selection proof",
               "vocat_profiles": len(vocat["profiles"]), "vocat_metadata_count": vocat["metadata"]["count"],
               "vocat_plmns": len(plmns), "switch_selectors": len(registry["selectors"]),
               "switch_profiles": len(registry["profiles"]),
               "shared_plmns": len(plmns & registry["selectors"].keys()),
               "reference_only_plmns": sorted(plmns - registry["selectors"].keys()),
               "reference_field_counts": dict(sorted(fields.items())), "synthetic_cases": len(cases),
               "reference_observations": len(observations), "automatic_imports": 0}
    if args.replay:
        replay = read_json(inside_workspace(args.replay))
        actual = {key(item["sim"]): item for item in replay}
        if len(actual) != len(cases) or set(actual) != {key(item["sim"]) for item in cases}:
            raise ValueError("replay does not cover exactly the generated synthetic cases")
        counts = Counter()
        for observation in observations:
            result = actual[key(observation["sim"])]
            profile = result.get("runtime_profile", {})
            effective = next(iter(profile.get("epdg_candidates", [])), standard_epdg(observation["sim"]))
            status = ("ambiguous_or_rejected" if result["rejected"] else
                      "no_switch_template" if not result["matched"] else
                      "reference_has_no_epdg" if not observation["reference_epdg"] else
                      "same_epdg" if effective == observation["reference_epdg"] else "conflict_keep_switch")
            observation.update({"switch_id": profile.get("id"), "switch_epdg": effective,
                                "status": status})
            counts[status] += 1
        differences = []
        comparable = 0
        for item in replay:
            profile = item.get("runtime_profile", {})
            if item["rejected"] or not item["matched"]:
                continue
            document = mit_profiles[profile["id"]]
            sim = item["sim"]
            configuration = copy.deepcopy(document.get("common", {}))
            merge(configuration, document.get("plmn_overrides", {}).get(sim["mcc"] + sim["mnc"], {}))
            merge(configuration, document.get("selector_overrides", {}).get(profile.get("selector_raw", ""), {}))
            current = next(iter(profile.get("epdg_candidates", [])), standard_epdg(sim))
            proposed = epdg(sim, configuration)
            comparable += 1
            if current != proposed:
                differences.append({"sim": sim, "profile": profile["id"], "current": current, "proposed": proposed})
        summary.update({"observation_status_counts": dict(sorted(counts.items())),
                        "replay_counts": replay_counts(replay),
                        "mit_epdg_comparable_cases": comparable, "mit_effective_epdg_changes": len(differences)})
        write_json(output / "reference-observations.json", observations)
        write_json(output / "mit-effective-epdg-differences.json", differences)
    write_json(output / "summary.json", summary)
    print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
