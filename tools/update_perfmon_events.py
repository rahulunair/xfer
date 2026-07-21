#!/usr/bin/env python3
"""Generate Rust Intel perfmon constants from pinned source extracts."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXTRACT_DIR = ROOT / "vendor" / "intel-perfmon" / "extracts"
EVENTS_JSON = EXTRACT_DIR / "selected-events.json"
MAPFILE_CSV = EXTRACT_DIR / "mapfile.csv"
OUTPUT = ROOT / "src" / "evidence" / "intel_perfmon" / "generated.rs"

UPSTREAM_REPOSITORY = "https://github.com/intel/perfmon"
UPSTREAM_COMMIT = "6e3329d20457aad11d8cc323b85aa6a16b075918"
MAPFILE_SHA256 = "5540502a0a2866fc9ff770ee56947859c7406136626afd4e79962b463434665d"

SOURCE_HASHES = {
    "SPR/events/sapphirerapids_uncore.json": "51eaed4092290ef9275a5b10a4ce9412b319cb19889e7c9ae9853f8b672e8a37",
    "SPR/events/sapphirerapids_uncore_experimental.json": "bf9b826e2bd7e8872d396377e32601dc21191655e465b57cfa16df3f4ce43771",
    "GNR/events/graniterapids_uncore.json": "690b0ee0fb6a7c8c5bce8a0d72f3ac8e493c8b5c042e3a650a6ee4174301d513",
    "GNR/events/graniterapids_uncore_experimental.json": "3d8500e8a5a7a8426c79e92f279219119099f288d425545d0f895144285df14f",
}

EXPECTED_HEADERS = {
    "SPR/events/sapphirerapids_uncore.json": {
        "Copyright": "Copyright (c) 2001 - 2026 Intel Corporation. All rights reserved.",
        "Info": "Performance Monitoring Events for 4th Generation Intel(R) Xeon(R) Processor Scalable Family based on Sapphire Rapids microarchitecture - V1.39",
        "DatePublished": "03/24/2026",
        "Version": "1.39",
        "Legend": "",
    },
    "SPR/events/sapphirerapids_uncore_experimental.json": {
        "Copyright": "Copyright (c) 2001 - 2026 Intel Corporation. All rights reserved.",
        "Info": "Performance Monitoring Events for 4th Generation Intel(R) Xeon(R) Processor Scalable Family based on Sapphire Rapids microarchitecture - V1.39",
        "DatePublished": "03/24/2026",
        "Version": "1.39",
        "Legend": "",
    },
    "GNR/events/graniterapids_uncore.json": {
        "Copyright": "Copyright (c) 2001 - 2026 Intel Corporation. All rights reserved.",
        "Info": "Performance Monitoring Events for Intel(R) Xeon(R) 6 Processor with P-cores - V1.20",
        "DatePublished": "06/05/2026",
        "Version": "1.20",
        "Legend": "",
    },
    "GNR/events/graniterapids_uncore_experimental.json": {
        "Copyright": "Copyright (c) 2001 - 2026 Intel Corporation. All rights reserved.",
        "Info": "Performance Monitoring Events for Intel(R) Xeon(R) 6 Processor with P-cores - V1.20",
        "DatePublished": "06/05/2026",
        "Version": "1.20",
        "Legend": "",
    },
}

TOP_LEVEL_KEYS = {"upstream", "sources"}
UPSTREAM_KEYS = {"repository", "commit", "license", "license_path"}
SOURCE_KEYS = {"path", "source_sha256", "header", "selected_event_names", "events"}
HEADER_KEYS = {"Copyright", "Info", "DatePublished", "Version", "Legend"}
EVENT_KEYS = {
    "BriefDescription",
    "Counter",
    "CounterType",
    "Deprecated",
    "ELLC",
    "EventCode",
    "EventName",
    "ExtSel",
    "FCMask",
    "FILTER_VALUE",
    "Filter",
    "PortMask",
    "PublicDescription",
    "UMask",
    "UMaskExt",
    "Unit",
}
MAPFILE_HEADER = [
    "Family-model",
    "Version",
    "Filename",
    "EventType",
    "Core Type",
    "Native Model ID",
    "Core Role Name",
]

SELECTED_EVENTS = {
    "SPR/events/sapphirerapids_uncore.json": [
        "UNC_IIO_DATA_REQ_OF_CPU.MEM_READ.ALL_PARTS",
        "UNC_IIO_DATA_REQ_OF_CPU.MEM_WRITE.ALL_PARTS",
        "UNC_UPI_TxL_FLITS.ALL_DATA",
        "UNC_UPI_RxL_FLITS.ALL_DATA",
    ],
    "SPR/events/sapphirerapids_uncore_experimental.json": [
        *[f"UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.PART{part}" for part in range(8)],
        *[f"UNC_IIO_DATA_REQ_BY_CPU.PEER_WRITE.PART{part}" for part in range(8)],
        *[f"UNC_IIO_DATA_REQ_BY_CPU.PEER_READ.PART{part}" for part in range(8)],
    ],
    "GNR/events/graniterapids_uncore.json": [
        "UNC_IIO_DATA_REQ_OF_CPU.MEM_READ.ALL_PARTS",
        "UNC_IIO_DATA_REQ_OF_CPU.MEM_WRITE.ALL_PARTS",
        "UNC_UPI_TxL_FLITS.ALL_DATA",
        "UNC_UPI_RxL_FLITS.ALL_DATA",
        "UNC_IIO_DATA_REQ_BY_CPU.PEER_WRITE.ALL_PARTS",
        "UNC_IIO_DATA_REQ_BY_CPU.PEER_READ.ALL_PARTS",
    ],
    "GNR/events/graniterapids_uncore_experimental.json": [
        "UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.ALL_PARTS",
        *[f"UNC_IIO_DATA_REQ_OF_CPU.PEER_READ.PART{part}" for part in range(8)],
    ],
}

SOURCE_RECORD_HASHES = {
    ("SPR/events/sapphirerapids_uncore.json", "UNC_IIO_DATA_REQ_OF_CPU.MEM_READ.ALL_PARTS"): "0312481a7d14b49fe9b2b8a05b14ad5c2ce86ff0f52ce8a0b0123018d29d1e62",
    ("SPR/events/sapphirerapids_uncore.json", "UNC_IIO_DATA_REQ_OF_CPU.MEM_WRITE.ALL_PARTS"): "b84aee00e2cae567bd5ba66ddccf8fd208650169f9979a7f37017b6a39b543db",
    ("SPR/events/sapphirerapids_uncore.json", "UNC_UPI_TxL_FLITS.ALL_DATA"): "e8c531d21e175554ae21e730414eddd126deed3b6c2b537edd43c116f44d7e5e",
    ("SPR/events/sapphirerapids_uncore.json", "UNC_UPI_RxL_FLITS.ALL_DATA"): "8db3281e861ef898f4f37f5ab716c47786c5c064ccbe8450dc0046ccabb9564c",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.PART0"): "fd4e7cfe368ccb0114ad17dc108b4526cb3eef0150f1ac731001129453187c9e",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.PART1"): "83a911ba21d5342face8f995ca3ba16a2059eaccd448040c3483be35ae819f94",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.PART2"): "dd79bd76bee9b807d1cf73527f951a6890ef5caf3751cd8062af5b420276cd0e",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.PART3"): "20ab0af0335eb1e841163c9a479631689cf7642cec6fcb53057a3bbda2bfca7a",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.PART4"): "f8e96c19ae057052f94245ea4df786a72d1bef319f76ff0875cdc82e8b1107fb",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.PART5"): "d1639643b1120b62e6d90044d765bc327046a8502a1ddc5413268e1f0f36d517",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.PART6"): "a7d9b166c21853c168e3be6f1aacb6ddc92121bee5f15286a47a38a607af6299",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.PART7"): "1b7c69659f160f80c981cbd093129a251916bdfc40e6291e7015fdc97550e853",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_WRITE.PART0"): "8a3f9a2e3a09acb6d602da77a004ecbd840191289903717713dae631dd804afb",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_WRITE.PART1"): "d01d79c4cd9d48977ea1aace19f08c8665235c3f76f71fbf5a071d8012727b6e",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_WRITE.PART2"): "0cb7dcc118c3252bb51bd1c3bcb31f507f247d03d9c128f98722c0ded988c154",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_WRITE.PART3"): "bc3d0d5f913acec79dff22cbf3e701ed47aa48ace2ece460d7625057da29e003",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_WRITE.PART4"): "e99135f3c3c79950ab7f7c5b821513b9848bbab8635d5b96a2b7f33f96fe8c78",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_WRITE.PART5"): "6b1b66c20afe240ea1d66b4a2eb98b1bb516fdf93d5cbacda7c29a1094c54399",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_WRITE.PART6"): "ff79112deb2d2ee6e400e9a22fc5b5ef6c8caaedf905c6226b75eabd1a36ab22",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_WRITE.PART7"): "e061e4c5a4b96edd3fd6689f20df4ed01910423a3fd25586f493ac09d2b43aa3",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_READ.PART0"): "8be38144849a129dfbad0ed1ab52e92fddc9a2d354203701f1196089cf02fedb",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_READ.PART1"): "9e4304aff9114c0a47f945fa9154338dab25e74b848fa2a09c7f746c3f6360dc",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_READ.PART2"): "b93de4ce75d412fb7796545b3e3f668bf2546e99b6e0f25cf40926974c3833c7",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_READ.PART3"): "bbb25430c7eb010b2393d1c4f7d84b47e7393fa2fb0aa33617c7f3385d1f6337",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_READ.PART4"): "f636f9f099df97a63b68c6cbef7860c2f4096e56eb6be8087042b61579fe5929",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_READ.PART5"): "6bffe8741e358e0b83d39cbae9a990cda8869afe36d87fa95afbf59469c3bcb1",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_READ.PART6"): "9b6c71a6abd1bf83e016ea41fca272c9845c712a7db25835a7d76bf9cfe1f4e7",
    ("SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_READ.PART7"): "fad422cfe7515982aeff10f4235ffdbc8914902e574afe7d9e01d482f5e1d51d",
    ("GNR/events/graniterapids_uncore.json", "UNC_IIO_DATA_REQ_OF_CPU.MEM_READ.ALL_PARTS"): "454d96ab91d507db188521cf087187f140129d572c83e5f48bb9aa5808dd3536",
    ("GNR/events/graniterapids_uncore.json", "UNC_IIO_DATA_REQ_OF_CPU.MEM_WRITE.ALL_PARTS"): "701ae12bb19fbd1491bd62e171daa9994d1c18c106a10c45bc2e1754527faf5e",
    ("GNR/events/graniterapids_uncore.json", "UNC_UPI_TxL_FLITS.ALL_DATA"): "f2dfdddca642726272ff8bc7657cefbcda5e83d7e28190b17779b761ee0d723a",
    ("GNR/events/graniterapids_uncore.json", "UNC_UPI_RxL_FLITS.ALL_DATA"): "6f88a7fe40ca00635f2527a576be8f44a31113b7f68f12622c9a22f09e8feea7",
    ("GNR/events/graniterapids_uncore.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_WRITE.ALL_PARTS"): "3ec45e1d803e391297512082857ca1753a95c34f23ab30f9d2596236519abc46",
    ("GNR/events/graniterapids_uncore.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_READ.ALL_PARTS"): "576afd76bd46e127f87ec8dd22cb81807408c381f78c42517c0b3f2dc13c071c",
    ("GNR/events/graniterapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.ALL_PARTS"): "2960a18b98cb904612fe1a450f001afd72ef9145fccedbb5e02daa6f660ea57e",
    ("GNR/events/graniterapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_READ.PART0"): "ecf029a523b24bc506dc0919e91ceefd8c40d8c3a96e2daa2848af9aed16ab7a",
    ("GNR/events/graniterapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_READ.PART1"): "86ea50424c23207c682a04418a524432c35451843dfe1e66984bdb1d990d8d47",
    ("GNR/events/graniterapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_READ.PART2"): "1bbfd1196c0b42f05be9712616fed92f1f3da2cbc88634afd404910596051822",
    ("GNR/events/graniterapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_READ.PART3"): "3257de5c55049e90ef7b6bdad5052ef8f481e9e8200a64c315f78fe40cba180a",
    ("GNR/events/graniterapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_READ.PART4"): "4ce6abab9a1902d6186a2f59b34467dcfa21ba191b0814e93f9ae18e15c87af7",
    ("GNR/events/graniterapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_READ.PART5"): "53fa6b29149970b25c0789a6869720915747a8783e7c594dd6a596f4623f12cf",
    ("GNR/events/graniterapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_READ.PART6"): "751ab15404391559b034d44124dd2612a25f29c2124b836b226bf1b1a4676076",
    ("GNR/events/graniterapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_READ.PART7"): "c1efe7276329f5cb4b37063da869fa5668142c29930471ddbdb494dfcc5910ae",
}

EXPECTED_MAP_ROWS = [
    ("GenuineIntel-6-8F", "V1.39", "/SPR/events/sapphirerapids_uncore.json", "uncore", 143, "12c6ef992e728b643d207505cd07e600a685ab0d5f44263afb471e67e769a728"),
    ("GenuineIntel-6-8F", "V1.39", "/SPR/events/sapphirerapids_uncore_experimental.json", "uncore experimental", 144, "381e88051f9e0fd21e85fa15de1ffd14dbaa63ec11d3871995499436cdc0367f"),
    ("GenuineIntel-6-AD", "V1.20", "/GNR/events/graniterapids_uncore.json", "uncore", 204, "bedd74545fccf4170c7868b58bc5ad23427b8920fcc1a4e5cac21667aa7e90d4"),
    ("GenuineIntel-6-AD", "V1.20", "/GNR/events/graniterapids_uncore_experimental.json", "uncore experimental", 205, "52f3002af7f416d3a1f8465bbc9345a3ac1d9a4586a35f8add1a058a541f7ab3"),
    ("GenuineIntel-6-AE", "V1.20", "/GNR/events/graniterapids_uncore.json", "uncore", 209, "79f5bf81b8e33374844e959e5ebd73cbe25cdce1678d3762d10cf2937344e7cb"),
    ("GenuineIntel-6-AE", "V1.20", "/GNR/events/graniterapids_uncore_experimental.json", "uncore experimental", 210, "140448054813f7bfbce1009f9fe4f75bd8b77b5ccb544b5481afb7112157a44a"),
]

DIRECT_OPERATIONAL_EVENTS = {
    "SPR": [
        ("SPR/events/sapphirerapids_uncore.json", "UNC_IIO_DATA_REQ_OF_CPU.MEM_READ.ALL_PARTS"),
        ("SPR/events/sapphirerapids_uncore.json", "UNC_IIO_DATA_REQ_OF_CPU.MEM_WRITE.ALL_PARTS"),
        ("SPR/events/sapphirerapids_uncore.json", "UNC_UPI_TxL_FLITS.ALL_DATA"),
        ("SPR/events/sapphirerapids_uncore.json", "UNC_UPI_RxL_FLITS.ALL_DATA"),
    ],
    "GNR": [
        ("GNR/events/graniterapids_uncore.json", "UNC_IIO_DATA_REQ_OF_CPU.MEM_READ.ALL_PARTS"),
        ("GNR/events/graniterapids_uncore.json", "UNC_IIO_DATA_REQ_OF_CPU.MEM_WRITE.ALL_PARTS"),
        ("GNR/events/graniterapids_uncore.json", "UNC_UPI_TxL_FLITS.ALL_DATA"),
        ("GNR/events/graniterapids_uncore.json", "UNC_UPI_RxL_FLITS.ALL_DATA"),
        ("GNR/events/graniterapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.ALL_PARTS"),
        ("GNR/events/graniterapids_uncore.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_WRITE.ALL_PARTS"),
        ("GNR/events/graniterapids_uncore.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_READ.ALL_PARTS"),
    ],
}

DERIVED_OPERATIONAL_EVENTS = {
    "SPR": [
        ("UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.ALL_PARTS", "SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.PART"),
        ("UNC_IIO_DATA_REQ_BY_CPU.PEER_WRITE.ALL_PARTS", "SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_WRITE.PART"),
        ("UNC_IIO_DATA_REQ_BY_CPU.PEER_READ.ALL_PARTS", "SPR/events/sapphirerapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_BY_CPU.PEER_READ.PART"),
    ],
    "GNR": [
        ("UNC_IIO_DATA_REQ_OF_CPU.PEER_READ.ALL_PARTS", "GNR/events/graniterapids_uncore_experimental.json", "UNC_IIO_DATA_REQ_OF_CPU.PEER_READ.PART"),
    ],
}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--import-from", type=Path, metavar="PERFMON_CHECKOUT")
    args = parser.parse_args()

    if args.import_from:
        import_from_checkout(args.import_from)

    data = load_and_validate_extracts()
    generated = rustfmt(generate(data))
    if args.check:
        current = OUTPUT.read_text()
        if current != generated:
            raise SystemExit(f"{OUTPUT} is stale; run tools/update_perfmon_events.py")
        return
    OUTPUT.write_text(generated)


def import_from_checkout(checkout: Path) -> None:
    commit = subprocess.run(
        ["git", "-C", str(checkout), "rev-parse", "HEAD"],
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    ).stdout.strip()
    if commit != UPSTREAM_COMMIT:
        raise SystemExit(f"expected perfmon commit {UPSTREAM_COMMIT}, got {commit}")

    if sha256_file(checkout / "mapfile.csv") != MAPFILE_SHA256:
        raise SystemExit("mapfile.csv hash does not match pinned source")

    rows = read_upstream_map_rows(checkout / "mapfile.csv")
    MAPFILE_CSV.write_text(render_mapfile_extract(rows))

    sources = []
    for path, expected_names in SELECTED_EVENTS.items():
        source_path = checkout / path
        source_hash = sha256_file(source_path)
        if source_hash != SOURCE_HASHES[path]:
            raise SystemExit(f"{path} hash does not match pinned source")
        source = load_json_no_duplicates(source_path)
        events_by_name = {event["EventName"]: event for event in source["Events"]}
        events = []
        for name in expected_names:
            events.append(events_by_name[name])
        sources.append(
            {
                "path": path,
                "source_sha256": source_hash,
                "header": source["Header"],
                "selected_event_names": expected_names,
                "events": events,
            }
        )

    data = {
        "upstream": {
            "repository": UPSTREAM_REPOSITORY,
            "commit": UPSTREAM_COMMIT,
            "license": "BSD-3-Clause",
            "license_path": "vendor/intel-perfmon/LICENSE",
        },
        "sources": sources,
    }
    EVENTS_JSON.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")


def load_and_validate_extracts() -> dict[str, object]:
    data = load_json_no_duplicates(EVENTS_JSON)
    require(set(data) == TOP_LEVEL_KEYS, "extract top-level schema mismatch")
    upstream = data["upstream"]
    require(set(upstream) == UPSTREAM_KEYS, "upstream metadata schema mismatch")
    require(upstream["repository"] == UPSTREAM_REPOSITORY, "bad upstream repository")
    require(upstream["commit"] == UPSTREAM_COMMIT, "bad upstream commit")
    require(upstream["license"] == "BSD-3-Clause", "bad upstream license")
    require(upstream["license_path"] == "vendor/intel-perfmon/LICENSE", "bad license path")

    validate_mapfile_extract()
    seen = set()
    sources = {}
    for source in data["sources"]:
        require(set(source) == SOURCE_KEYS, "source metadata schema mismatch")
        path = source["path"]
        require(path not in sources, f"duplicate source {path}")
        sources[path] = source
    require(set(sources) == set(SELECTED_EVENTS), "source set mismatch")
    for path, expected_names in SELECTED_EVENTS.items():
        source = sources[path]
        require(set(source["header"]) == HEADER_KEYS, f"{path} header schema mismatch")
        require(source["source_sha256"] == SOURCE_HASHES[path], f"{path} source hash mismatch")
        require(source["header"] == EXPECTED_HEADERS[path], f"{path} header mismatch")
        validate_exact_name_set(source["selected_event_names"], expected_names, f"{path} selected")
        require(len(source["events"]) == len(expected_names), f"{path} event count mismatch")
        events_by_name = {}
        actual_event_names = []
        for event in source["events"]:
            require(set(event) == EVENT_KEYS, f"{path} event schema mismatch")
            name = event["EventName"]
            require(name not in events_by_name, f"duplicate selected event name {path} {name}")
            events_by_name[name] = event
            actual_event_names.append(name)
            require((path, name) not in seen, f"duplicate selected event {path} {name}")
            seen.add((path, name))
            actual_hash = sha256_json(event)
            require(
                actual_hash == SOURCE_RECORD_HASHES[(path, name)],
                f"{path} {name} record hash mismatch",
            )
            validate_event_semantics(path, event)
        require(actual_event_names == expected_names, f"{path} event order mismatch")
    return data


def validate_event_semantics(path: str, event: dict[str, str]) -> None:
    name = event["EventName"]
    require(event["CounterType"] == "PGMABLE", f"{path} {name} CounterType must be PGMABLE")
    require(event["Deprecated"] == "0", f"{path} {name} must not be deprecated")
    require(event["Filter"] == "na", f"{path} {name} Filter must be na")
    require(event["ELLC"] == "0", f"{path} {name} ELLC must remain zero")
    require(event["Unit"] in {"IIO", "UPI LL"}, f"{path} {name} unsupported Unit")
    parse_counter_list(event["Counter"])
    for field in ["EventCode", "UMask", "PortMask", "FCMask", "UMaskExt", "ExtSel", "FILTER_VALUE"]:
        parse_int(event[field])
    require(event["ExtSel"] == "0", f"{path} {event['EventName']} ExtSel must remain zero")
    require(
        event["FILTER_VALUE"] == "0",
        f"{path} {event['EventName']} FILTER_VALUE must remain zero",
    )
    umask_ext = parse_int(event["UMaskExt"])
    if umask_ext != 0:
        expected = (parse_int(event["FCMask"]) << 16) | (parse_int(event["PortMask"]) << 4)
        require(
            umask_ext == expected,
            f"{path} {event['EventName']} UMaskExt relation changed",
        )


def validate_mapfile_extract() -> None:
    lines = MAPFILE_CSV.read_text().splitlines()
    require(lines, "empty mapfile extract")
    reader = csv.reader(lines)
    header = next(reader)
    require(header == MAPFILE_HEADER, "mapfile header mismatch")

    actual_rows = []
    seen_identities = set()
    for row in reader:
        require(len(row) == len(MAPFILE_HEADER), "mapfile row width mismatch")
        raw = ",".join(row) + "\n"
        identity = (row[0], row[1], row[2], row[3])
        require(identity not in seen_identities, f"duplicate mapfile row {identity}")
        seen_identities.add(identity)
        actual_rows.append((*identity, sha256_bytes(raw.encode())))

    expected_rows = [
        (family_model, version, filename, event_type, row_hash)
        for family_model, version, filename, event_type, _line, row_hash in EXPECTED_MAP_ROWS
    ]
    require(actual_rows == expected_rows, "mapfile row order or hash mismatch")


def generate(data: dict[str, object]) -> str:
    sources_by_path = {source["path"]: source for source in data["sources"]}
    events = {
        (source["path"], event["EventName"]): event
        for source in data["sources"]
        for event in source["events"]
    }

    support_consts = []
    source_consts = []
    event_consts = []
    profile_events = {"SPR": [], "GNR": []}

    for source_index, path in enumerate(SELECTED_EVENTS):
        source = sources_by_path[path]
        event_names_const = f"SOURCE_{source_index}_EVENT_NAMES"
        support_consts.append(static_str_slice(event_names_const, SELECTED_EVENTS[path]))
        source_consts.append(render_source(source, event_names_const))

    for profile, selections in DIRECT_OPERATIONAL_EVENTS.items():
        for path, name in selections:
            const_name = event_const_name(profile, name)
            event_consts.append(
                render_event_const(
                    const_name,
                    role_expr(name),
                    events[(path, name)],
                    direct_provenance(path, name),
                )
            )
            profile_events[profile].append(const_name)

    for profile, derived_specs in DERIVED_OPERATIONAL_EVENTS.items():
        for target_name, path, prefix in derived_specs:
            derived = derive_union(profile, target_name, path, prefix, events)
            support_consts.extend(derived["support"])
            event_consts.append(
                render_event_const(
                    derived["const"],
                    role_expr(target_name),
                    derived["event"],
                    derived["provenance"],
                )
            )
            profile_events[profile].append(derived["const"])

    out = [
        "// @generated by tools/update_perfmon_events.py; do not edit by hand.\n",
        f"// Source: {UPSTREAM_REPOSITORY} @ {UPSTREAM_COMMIT}; license: BSD-3-Clause in vendor/intel-perfmon/LICENSE.\n",
        "use super::{\n",
        "    CounterRestriction, EventProvenance, EventRole, MapfileRow, PerfmonAttribution,\n",
        "    PerfmonEvent, PerfmonProfile, PerfmonProfileId, PerfmonSource, PerfmonSourceKind,\n",
        "    PerfmonUnit, ProgrammableCounter, RuntimeField, RuntimeFieldValue, SourceField,\n",
        "};\n\n",
        f"pub const UPSTREAM_COMMIT: &str = {rust_str(UPSTREAM_COMMIT)};\n\n",
        "pub const ATTRIBUTION: PerfmonAttribution = PerfmonAttribution {\n",
        f"    upstream_repository: {rust_str(UPSTREAM_REPOSITORY)},\n",
        f"    upstream_commit: {rust_str(UPSTREAM_COMMIT)},\n",
        '    license: "BSD-3-Clause",\n',
        '    license_path: "vendor/intel-perfmon/LICENSE",\n',
        "};\n\n",
        *support_consts,
        *render_map_rows(),
        *event_consts,
        static_event_slice("SPR_EVENTS", profile_events["SPR"]),
        static_event_slice("GNR_EVENTS", profile_events["GNR"]),
        "const SPR_CPU_MODELS: &[u16] = &[0x8f];\n",
        "const GNR_CPU_MODELS: &[u16] = &[0xad, 0xae];\n",
        "const SPR_MAP_ROWS: &[MapfileRow] = &[MAP_ROW_SPR_UNCORE, MAP_ROW_SPR_UNCORE_EXPERIMENTAL];\n",
        "const GNR_MAP_ROWS: &[MapfileRow] = &[\n",
        "    MAP_ROW_GNR_AD_UNCORE,\n",
        "    MAP_ROW_GNR_AD_UNCORE_EXPERIMENTAL,\n",
        "    MAP_ROW_GNR_AE_UNCORE,\n",
        "    MAP_ROW_GNR_AE_UNCORE_EXPERIMENTAL,\n",
        "];\n",
        "pub const PROFILES: &[PerfmonProfile] = &[\n",
        "    PerfmonProfile { id: PerfmonProfileId::SapphireRapids, cpu_models: SPR_CPU_MODELS, mapfile_rows: SPR_MAP_ROWS, events: SPR_EVENTS },\n",
        "    PerfmonProfile { id: PerfmonProfileId::GraniteRapids, cpu_models: GNR_CPU_MODELS, mapfile_rows: GNR_MAP_ROWS, events: GNR_EVENTS },\n",
        "];\n\n",
        "pub const SOURCES: &[PerfmonSource] = &[\n",
        *[f"    {source},\n" for source in source_consts],
        "];\n\n",
        "pub const MAPFILE_ROWS: &[MapfileRow] = &[\n",
        "    MAP_ROW_SPR_UNCORE,\n",
        "    MAP_ROW_SPR_UNCORE_EXPERIMENTAL,\n",
        "    MAP_ROW_GNR_AD_UNCORE,\n",
        "    MAP_ROW_GNR_AD_UNCORE_EXPERIMENTAL,\n",
        "    MAP_ROW_GNR_AE_UNCORE,\n",
        "    MAP_ROW_GNR_AE_UNCORE_EXPERIMENTAL,\n",
        "];\n",
    ]
    return "".join(out)


def render_source(source: dict[str, object], event_names_const: str) -> str:
    return (
        "PerfmonSource {\n"
        f"        path: {rust_str(source['path'])},\n"
        f"        kind: {source_kind(source['path'])},\n"
        f"        copyright: {rust_str(source['header']['Copyright'])},\n"
        f"        info: {rust_str(source['header']['Info'])},\n"
        f"        event_db_version: {rust_str(source['header']['Version'])},\n"
        f"        date_published: {rust_str(source['header']['DatePublished'])},\n"
        f"        upstream_sha256: {rust_str(source['source_sha256'])},\n"
        f"        selected_event_names: {event_names_const},\n"
        "    }"
    )


def render_map_rows() -> list[str]:
    names = [
        "MAP_ROW_SPR_UNCORE",
        "MAP_ROW_SPR_UNCORE_EXPERIMENTAL",
        "MAP_ROW_GNR_AD_UNCORE",
        "MAP_ROW_GNR_AD_UNCORE_EXPERIMENTAL",
        "MAP_ROW_GNR_AE_UNCORE",
        "MAP_ROW_GNR_AE_UNCORE_EXPERIMENTAL",
    ]
    blocks = []
    for const_name, expected in zip(names, EXPECTED_MAP_ROWS, strict=True):
        family_model, version, filename, event_type, source_line, row_hash = expected
        blocks.append(
            f"const {const_name}: MapfileRow = MapfileRow {{\n"
            f"    family_model: {rust_str(family_model)},\n"
            f"    version: {rust_str(version)},\n"
            f"    filename: {rust_str(filename)},\n"
            f"    event_type: {source_kind_from_text(event_type)},\n"
            f"    source_line: {source_line},\n"
            f"    source_row_sha256: {rust_str(row_hash)},\n"
            "};\n\n"
        )
    return blocks


def render_event_const(
    const_name: str,
    role: str,
    event: dict[str, str],
    provenance: str,
) -> str:
    runtime_fields_const = f"{const_name}_RUNTIME_FIELDS"
    source_fields_const = f"{const_name}_SOURCE_FIELDS"
    counters_const = f"{const_name}_COUNTERS"
    return (
        static_counter_slice(counters_const, parse_counter_list(event["Counter"]))
        + f"const {runtime_fields_const}: &[RuntimeFieldValue] = &[\n"
        + runtime_field("EventCode", event["EventCode"])
        + runtime_field("UMask", event["UMask"])
        + runtime_field("PortMask", event["PortMask"])
        + runtime_field("FCMask", event["FCMask"])
        + "];\n"
        + f"const {source_fields_const}: &[SourceField] = &[\n"
        + "".join(
            f"    SourceField {{ name: {rust_str(name)}, value: {rust_str(str(value))} }},\n"
            for name, value in event.items()
        )
        + "];\n"
        + f"const {const_name}: PerfmonEvent = PerfmonEvent {{\n"
        + f"    role: {role},\n"
        + f"    unit: {unit_expr(event['Unit'])},\n"
        + f"    brief_description: {rust_str(event['BriefDescription'])},\n"
        + f"    public_description: {rust_str(event['PublicDescription'])},\n"
        + "    counter: CounterRestriction {\n"
        + f"        raw: {rust_str(event['Counter'])},\n"
        + f"        counters: {counters_const},\n"
        + "    },\n"
        + f"    runtime_fields: {runtime_fields_const},\n"
        + f"    source_fields: {source_fields_const},\n"
        + f"    provenance: {provenance},\n"
        + "};\n\n"
    )


def runtime_field(field: str, raw: str) -> str:
    return (
        "    RuntimeFieldValue { "
        f"field: RuntimeField::{field}, "
        f"value: 0x{parse_int(raw):x}, "
        f"raw: {rust_str(raw)} "
        "},\n"
    )


def derive_union(
    profile: str,
    target_name: str,
    path: str,
    prefix: str,
    events: dict[tuple[str, str], dict[str, str]],
) -> dict[str, object]:
    names = [f"{prefix}{part}" for part in range(8)]
    parts = [events[(path, name)] for name in names]
    first = parts[0]
    for event in parts:
        for key in [
            "Unit",
            "EventCode",
            "UMask",
            "FCMask",
            "ExtSel",
            "FILTER_VALUE",
            "Counter",
            "CounterType",
            "Deprecated",
            "ELLC",
            "Filter",
        ]:
            require(event[key] == first[key], f"{target_name} cannot derive union; {key} differs")

    port_mask = 0
    umask_ext = 0
    for event in parts:
        port_mask |= parse_int(event["PortMask"])
        umask_ext |= parse_int(event["UMaskExt"])
    require(port_mask == 0xFF, f"{target_name} derived PortMask is not 0xff")
    if umask_ext != 0:
        expected = (parse_int(first["FCMask"]) << 16) | (port_mask << 4)
        require(umask_ext == expected, f"{target_name} derived UMaskExt relation changed")

    event = dict(first)
    event["EventName"] = f"DERIVED_{target_name}"
    event["PortMask"] = "0x00ff"
    event["UMaskExt"] = f"0x{umask_ext:08X}"
    event["BriefDescription"] = f"Derived union of {prefix}0..{prefix}7"
    event["PublicDescription"] = (
        "Derived by verifying shared non-mask encoding/counter fields across eight "
        "source part records, then OR-ing PortMask and validating redundant UMaskExt."
    )
    names_const = f"{profile}_{event_const_name(profile, target_name)}_SOURCE_NAMES"
    hashes_const = f"{profile}_{event_const_name(profile, target_name)}_SOURCE_HASHES"
    support = [
        static_str_slice(names_const, names),
        static_str_slice(hashes_const, [SOURCE_RECORD_HASHES[(path, name)] for name in names]),
    ]
    provenance = (
        "EventProvenance::DerivedUnion {\n"
        f"        source_path: {rust_str(path)},\n"
        f"        source_event_names: {names_const},\n"
        f"        source_record_sha256: {hashes_const},\n"
        "        rule: \"verified shared non-mask encoding/counter fields across PART0..PART7; OR PortMask; validate redundant UMaskExt\",\n"
        "    }"
    )
    return {
        "const": f"{profile}_DERIVED_{event_const_name(profile, target_name)}",
        "event": event,
        "provenance": provenance,
        "support": support,
    }


def direct_provenance(path: str, name: str) -> str:
    return (
        "EventProvenance::Direct {\n"
        f"        source_path: {rust_str(path)},\n"
        f"        source_event_name: {rust_str(name)},\n"
        f"        source_record_sha256: {rust_str(SOURCE_RECORD_HASHES[(path, name)])},\n"
        "    }"
    )


def read_upstream_map_rows(path: Path) -> list[dict[str, str]]:
    expected_by_key = {
        (family_model, filename): expected
        for family_model, _version, filename, _event_type, _line, _row_hash in EXPECTED_MAP_ROWS
        for expected in [None]
    }
    rows_by_key = {}
    with path.open(newline="") as f:
        for row in csv.DictReader(f):
            key = (row["Family-model"], row["Filename"])
            if key in expected_by_key:
                require(key not in rows_by_key, f"duplicate selected mapfile row {key}")
                rows_by_key[key] = row
    require(len(rows_by_key) == len(EXPECTED_MAP_ROWS), "selected mapfile rows missing")
    return [
        rows_by_key[(family_model, filename)]
        for family_model, _version, filename, _event_type, _line, _row_hash in EXPECTED_MAP_ROWS
    ]


def render_mapfile_extract(rows: list[dict[str, str]]) -> str:
    header = ",".join(MAPFILE_HEADER) + "\n"
    body = "".join(",".join(row[field] for field in MAPFILE_HEADER) + "\n" for row in rows)
    return header + body


def rustfmt(source: str) -> str:
    with tempfile.NamedTemporaryFile("w+", suffix=".rs") as rust_file:
        rust_file.write(source)
        rust_file.flush()
        result = subprocess.run(
            ["rustfmt", "--edition", "2024", "--emit", "stdout", rust_file.name],
            check=True,
            stdout=subprocess.PIPE,
            text=True,
        )
    header = f"{rust_file.name}:\n"
    if result.stdout.startswith(header):
        return result.stdout[len(header) :].lstrip("\n")
    return result.stdout


def static_counter_slice(name: str, counters: list[int]) -> str:
    values = ", ".join(f"ProgrammableCounter::new({counter})" for counter in counters)
    return f"const {name}: &[ProgrammableCounter] = &[{values}];\n"


def static_str_slice(name: str, values: list[str]) -> str:
    body = "".join(f"    {rust_str(value)},\n" for value in values)
    return f"const {name}: &[&str] = &[\n{body}];\n"


def static_event_slice(name: str, values: list[str]) -> str:
    body = "".join(f"    {value},\n" for value in values)
    return f"const {name}: &[PerfmonEvent] = &[\n{body}];\n"


def event_const_name(profile: str, event_name: str) -> str:
    name = event_name.removeprefix("DERIVED_").replace(".", "_").replace("-", "_").upper()
    return f"{profile}_{name}"


def role_expr(event_name: str) -> str:
    name = event_name.removeprefix("DERIVED_")
    if name == "UNC_IIO_DATA_REQ_OF_CPU.MEM_READ.ALL_PARTS":
        return "EventRole::IioDataReqOfCpuMemReadAllParts"
    if name == "UNC_IIO_DATA_REQ_OF_CPU.MEM_WRITE.ALL_PARTS":
        return "EventRole::IioDataReqOfCpuMemWriteAllParts"
    if name == "UNC_IIO_DATA_REQ_OF_CPU.PEER_WRITE.ALL_PARTS":
        return "EventRole::IioDataReqOfCpuPeerWriteAllParts"
    if name == "UNC_IIO_DATA_REQ_OF_CPU.PEER_READ.ALL_PARTS":
        return "EventRole::IioDataReqOfCpuPeerReadAllParts"
    if name == "UNC_IIO_DATA_REQ_BY_CPU.PEER_WRITE.ALL_PARTS":
        return "EventRole::IioDataReqByCpuPeerWriteAllParts"
    if name == "UNC_IIO_DATA_REQ_BY_CPU.PEER_READ.ALL_PARTS":
        return "EventRole::IioDataReqByCpuPeerReadAllParts"
    if name == "UNC_UPI_TxL_FLITS.ALL_DATA":
        return "EventRole::UpiTxDataFlitsAll"
    if name == "UNC_UPI_RxL_FLITS.ALL_DATA":
        return "EventRole::UpiRxDataFlitsAll"
    raise SystemExit(f"unhandled event role for {event_name}")


def unit_expr(unit: str) -> str:
    if unit == "IIO":
        return "PerfmonUnit::Iio"
    if unit == "UPI LL":
        return "PerfmonUnit::UpiLl"
    raise SystemExit(f"unhandled unit {unit}")


def source_kind(path: str) -> str:
    if path.endswith("_experimental.json"):
        return "PerfmonSourceKind::UncoreExperimental"
    return "PerfmonSourceKind::Uncore"


def source_kind_from_text(kind: str) -> str:
    if kind == "uncore":
        return "PerfmonSourceKind::Uncore"
    if kind == "uncore experimental":
        return "PerfmonSourceKind::UncoreExperimental"
    raise SystemExit(f"unhandled source kind {kind}")


def parse_counter_list(text: str) -> list[int]:
    require(text, "empty counter list")
    counters = []
    for part in text.split(","):
        require(part.isdigit(), f"invalid counter id {part!r}")
        counter = int(part)
        require(0 <= counter <= 7, f"counter id {counter} out of supported range")
        require(counter not in counters, f"duplicate counter id {counter}")
        counters.append(counter)
    return counters


def parse_int(text: str) -> int:
    if text.lower().startswith("0x"):
        return int(text, 16)
    return int(text)


def sha256_json(value: object) -> str:
    return sha256_bytes(json.dumps(value, sort_keys=True, separators=(",", ":")).encode())


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def load_json_no_duplicates(path: Path) -> dict[str, object]:
    def reject_duplicate_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
        result = {}
        for key, value in pairs:
            require(key not in result, f"{path} contains duplicate JSON key {key!r}")
            result[key] = value
        return result

    return json.loads(path.read_text(), object_pairs_hook=reject_duplicate_keys)


def validate_exact_name_set(actual: list[str], expected: list[str], context: str) -> None:
    seen = set()
    for name in actual:
        require(name not in seen, f"duplicate {context} name {name}")
        seen.add(name)
    require(actual == expected, f"{context} name order mismatch")


def rust_str(value: str) -> str:
    return json.dumps(value)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(message)


if __name__ == "__main__":
    main()
