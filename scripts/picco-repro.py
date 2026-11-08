"""Compare relocated clean firmware builds, retaining inputs, logs and artifacts."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    root = Path(__file__).resolve().parent.parent
    output = root / "output"
    output.mkdir(exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="repro-current-", dir=output))
    report = {"status": "running", "run": str(run), "builds": [], "comparisons": {}}

    def save():
        temporary = run / "report.tmp"
        temporary.write_text(json.dumps(report, indent=2) + "\n")
        temporary.replace(run / "report.json")

    print(f"Reproducibility run: {run}", flush=True)
    save()
    try:
        listed = subprocess.check_output(
            ["rg", "--files", "--hidden", "-0", "-g", "!.git/**"], cwd=root
        ).split(b"\0")
        paths = sorted(os.fsdecode(path) for path in listed if path)
        for path in paths:
            if (root / path).is_symlink():
                raise RuntimeError(f"Source symlink requires explicit handling: {path}")
        inputs = {path: digest(root / path) for path in paths}
        (run / "inputs.json").write_text(json.dumps(inputs, indent=2) + "\n")
        report["inputs_sha256"] = digest(run / "inputs.json")
        report["input_count"] = len(inputs)
        sources = []
        for name in ("a", "b"):
            source = run / f"source-{name}"
            for path in paths:
                target = source / path
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(root / path, target)
                if digest(target) != inputs[path]:
                    raise RuntimeError(f"Input changed during copy: {path}")
            sources.append(source)
        artifacts = ("femto-picco.elf", "femto-picco.bin", "femto-picco.json", "femto-picco.debug.elf")
        for levels in (2, 4):
            results = []
            for name, source in zip(("a", "b"), sources):
                target = run / f"target-{levels}-{name}"
                destination = run / f"artifacts-{levels}-{name}"
                if target.exists() or destination.exists():
                    raise RuntimeError("Clean target or artifact directory already exists")
                log = run / f"build-{levels}-{name}.log"
                build = {"levels": levels, "source": str(source), "target": str(target),
                         "artifacts": str(destination), "log": str(log),
                         "target_absent_before_build": True, "started_at": time.time()}
                report["builds"].append(build)
                save()
                env = dict(os.environ, CARGO_TARGET_DIR=str(target), CARGO_BUILD_JOBS="1",
                           FEMTO_PICCO_OUTPUT=str(destination), FEMTO_PICCO_GRAYSCALE=str(int(levels == 4)))
                for key in ("FEMTO_IDF_SHELL", "FEMTO_PICCO_BUILD_NAMESPACE"):
                    env.pop(key, None)
                print(f"Building {levels} levels from source {name}; log: {log}", flush=True)
                with log.open("wb") as stream:
                    process = subprocess.run(["./scripts/build-picco"], cwd=source, env=env,
                                             stdout=stream, stderr=subprocess.STDOUT)
                build.update(exit_code=process.returncode, finished_at=time.time(), log_sha256=digest(log))
                save()
                if process.returncode:
                    raise RuntimeError(f"Build failed with {process.returncode}: {log}")
                results.append({name: digest(destination / name) for name in artifacts})
            comparison = {}
            for name in artifacts:
                comparison[name] = {"sha256": results[0][name], "equal": results[0][name] == results[1][name]}
            report["comparisons"][str(levels)] = comparison
            save()
            if not all(item["equal"] for item in comparison.values()):
                raise RuntimeError(f"Artifacts differ for {levels} levels")
        for source in [root, *sources]:
            for path, expected in inputs.items():
                if digest(source / path) != expected:
                    raise RuntimeError(f"Input changed during builds: {source / path}")
        report.update(status="passed", inputs_unchanged=True, finished_at=time.time())
        save()
        print(f"Both display variants reproduce across relocated clean builds: {run / 'report.json'}", flush=True)
    except BaseException as error:
        report.update(status="failed", error=str(error), finished_at=time.time())
        save()
        raise


if __name__ == "__main__":
    main()
