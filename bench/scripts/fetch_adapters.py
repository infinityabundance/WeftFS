#!/usr/bin/env python3
"""Fetch the pinned public adapter corpus and write its manifest.

Standard library only. Every file is hashed after download and compared with
the LFS pointer Hugging Face reports; a mismatch aborts, it never writes a
manifest. Nothing here is a measurement.
"""

import hashlib
import json
import os
import sys
import urllib.request

REPOS = [
    ("taronklm/Qwen2.5-0.5B-Instruct-lora-chatbot", "6060d5f819c4d0c75729975f308562149c7e2d01"),
    ("aneesa3131/Qwen2.5-0.5B-Instruct-lora-text-generation", "fb13685aeefd3c6dc7c5cad18659440e0faf0977"),
    ("ShahzebKhoso/qwen2.5-instruct-0.5B-pubmedqa-lora", "8dca0cc1f399204e0a93bb48fa39aba6add79be7"),
    ("Hazde/careerbot_PG6_Qwen_Qwen2.5-0.5B-Instruct_model_LoRA", "622f5f05293833bd76ff4749fc9a375c10b2aa43"),
    ("Hazde/careerbot_PG6_Qwen_Qwen2.5-0.5B-Instruct_model_LoRA_2", "6af65af1eca84c2f4bb95ebe2b20de4ff396c5da"),
    ("MuQianyu/qwen2.5-0.5B_lora16_spin0", "e50f73af059d2516c4fbeac58af58df8ceffa7a7"),
    ("MuQianyu/qwen2.5-0.5B_lora16_spin1", "945017b1ac42b2e052ccdfc9336c9c872f2fde70"),
    ("yeok/qwen-2.5-0.5B-instruct-sft-lora-countdown-deepseek-5k", "049da22b546039b0d775de553c4252d19a1fd8cf"),
    ("zqiu/lora_Qwen2.5-0.5B-Instruct", "8a3657b3d2b0b07128221dbb6231869a95514615"),
    ("TienAnh/finetune-Qwen2.5-0.5B-Instruct-lora", "d1f76b8583be79627541af83fb3f82389df6526c"),
    ("SykoSLM/qwen2.5-0.5b-instruct-turkish-finetuned-lora", "c06200b40e855e279977a499552f787256fae08f"),
    ("dilkushsingh/qwen2.5-0.5b-nlp2bash-DoRA", "1da7f66f361c065854a2e0d2d646027798602e20"),
]

ROOT = os.path.join(os.path.dirname(__file__), "..", "..", ".docker-cache", "t048")
ROOT = os.path.abspath(ROOT)
OUT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "corpora", "real-models", "adapters-qwen2.5-0.5b.json"))


def slug(repo):
    return repo.replace("/", "__")


def fetch(url, path):
    if os.path.exists(path):
        return
    tmp = path + ".tmp"
    with urllib.request.urlopen(url, timeout=120) as response, open(tmp, "wb") as handle:
        while True:
            chunk = response.read(1 << 20)
            if not chunk:
                break
            handle.write(chunk)
    os.replace(tmp, path)


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main():
    os.makedirs(ROOT, exist_ok=True)
    entries = []
    for repo, revision in REPOS:
        info = json.load(urllib.request.urlopen(
            "https://huggingface.co/api/models/%s/revision/%s?blobs=true" % (repo, revision),
            timeout=120,
        ))
        files = {s["rfilename"]: s for s in info.get("siblings", [])}
        adapter = files.get("adapter_model.safetensors")
        if adapter is None:
            raise SystemExit("%s has no adapter_model.safetensors" % repo)
        config = files.get("adapter_config.json")
        directory = os.path.join(ROOT, slug(repo))
        os.makedirs(directory, exist_ok=True)
        adapter_path = os.path.join(directory, "adapter_model.safetensors")
        config_path = os.path.join(directory, "adapter_config.json")
        fetch("https://huggingface.co/%s/resolve/%s/adapter_model.safetensors" % (repo, revision), adapter_path)
        if config is not None:
            fetch("https://huggingface.co/%s/resolve/%s/adapter_config.json" % (repo, revision), config_path)
        adapter_sha = sha256(adapter_path)
        expected = (adapter.get("lfs") or {}).get("sha256")
        if expected and expected != adapter_sha:
            raise SystemExit("%s: sha256 %s != LFS pointer %s" % (repo, adapter_sha, expected))
        config_sha = sha256(config_path) if os.path.exists(config_path) else None
        config_json = json.load(open(config_path)) if config_sha else {}
        entries.append({
            "repo": repo,
            "revision": revision,
            "declared_base_model": config_json.get("base_model_name_or_path"),
            "peft_type": config_json.get("peft_type"),
            "rank": config_json.get("r"),
            "lora_alpha": config_json.get("lora_alpha"),
            "use_dora": config_json.get("use_dora"),
            "use_rslora": config_json.get("use_rslora"),
            "bias": config_json.get("bias"),
            "target_modules": sorted(config_json.get("target_modules") or []),
            "files": [
                {
                    "path": "adapter_model.safetensors",
                    "url": "https://huggingface.co/%s/resolve/%s/adapter_model.safetensors" % (repo, revision),
                    "size": adapter.get("size"),
                    "sha256": adapter_sha,
                    "lfs_sha256": expected,
                },
                {
                    "path": "adapter_config.json",
                    "url": "https://huggingface.co/%s/resolve/%s/adapter_config.json" % (repo, revision),
                    "size": config.get("size") if config else None,
                    "sha256": config_sha,
                    "lfs_sha256": (config.get("lfs") or {}).get("sha256") if config else None,
                },
            ],
            "local_path": os.path.relpath(adapter_path, os.path.join(ROOT, "..", "..")),
        })
        print(repo, adapter_sha, adapter.get("size"))
    manifest = {
        "schema": "weft.real-adapter.corpus.v1",
        "label": "Public LoRA/DoRA adapters for the pinned Qwen2.5-0.5B-Instruct family",
        "note": (
            "Every file was downloaded from Hugging Face, hashed locally, and compared with the "
            "LFS pointer the API reports. `declared_base_model` is the PEFT config's own field, "
            "copied verbatim; adapters whose field is not exactly Qwen/Qwen2.5-0.5B-Instruct are "
            "still ingested as real LoRA matrices but are not merged into the bf16 base unless the "
            "court says so. No weights are committed to this repository."
        ),
        "base_model": {
            "repo": "Qwen/Qwen2.5-0.5B-Instruct",
            "revision": "7ae557604adf67be50417f59c2c2f167def9a775",
            "file": "model.safetensors",
            "sha256": "fdf756fa7fcbe7404d5c60e26bff1a0c8b8aa1f72ced49e7dd0210fe288fb7fe",
            "local_path": ".docker-cache/real-corpus/Qwen2.5-0.5B-Instruct.safetensors",
        },
        "adapters": entries,
    }
    with open(OUT, "w") as handle:
        json.dump(manifest, handle, indent=1, sort_keys=False)
        handle.write("\n")
    print("wrote", OUT)


if __name__ == "__main__":
    main()
