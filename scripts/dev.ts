#!/usr/bin/env bun

import { $ } from "bun";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const appName = "Waku Debug";
const appPath = join(root, "target/debug/Waku Debug.app");
const watchedDirectories = ["src", "assets", "resources", "vendor"];
const watchedFiles = ["Cargo.toml", "Cargo.lock"];

$.cwd(root);

let app: ReturnType<typeof Bun.spawn> | undefined;
let stopping = false;

async function sourceSignature(): Promise<string> {
  const paths = [...watchedFiles];
  const glob = new Bun.Glob("**/*");

  for (const directory of watchedDirectories) {
    for await (const path of glob.scan({
      cwd: join(root, directory),
      dot: true,
      onlyFiles: true,
    })) {
      paths.push(join(directory, path));
    }
  }

  const hash = new Bun.CryptoHasher("sha256");
  for (const path of paths.sort()) {
    hash.update(path);
    hash.update(await Bun.file(join(root, path)).arrayBuffer());
  }
  return hash.digest("hex");
}

async function build(): Promise<boolean> {
  console.log("[waku-dev] Building app bundle...");
  const result = await $`${join(root, "scripts/bundle.sh")} debug`.nothrow();
  if (result.exitCode !== 0) {
    console.error("[waku-dev] Build failed; keeping the current app open.");
    return false;
  }
  return true;
}

async function stopApp(): Promise<void> {
  const waiter = app;
  app = undefined;
  await $`pkill -TERM -x ${appName}`.quiet().nothrow();
  if (waiter?.exitCode === null) {
    await waiter.exited;
  }
}

function launchApp(): ReturnType<typeof Bun.spawn> {
  console.log(`[waku-dev] Launching ${appPath}`);
  return Bun.spawn(["open", "-n", "-W", appPath], {
    stdout: "inherit",
    stderr: "inherit",
  });
}

async function cleanup(): Promise<void> {
  if (stopping) return;
  stopping = true;
  console.log("[waku-dev] Stopping watcher and app...");
  await stopApp();
}

process.on("SIGINT", () => void cleanup());
process.on("SIGTERM", () => void cleanup());

let signature = await sourceSignature();
if (!(await build())) {
  process.exit(1);
}

await stopApp();
app = launchApp();
signature = await sourceSignature();
console.log(
  "[waku-dev] Watching for source changes. Quit the app or press Ctrl-C to stop.",
);

while (!stopping && app) {
  const currentApp = app;
  const event = await Promise.race([
    currentApp.exited.then((exitCode) => ({ exitCode, type: "exit" as const })),
    Bun.sleep(500).then(() => ({ type: "tick" as const })),
  ]);

  if (event.type === "exit") {
    if (app === currentApp) {
      app = undefined;
      console.log("[waku-dev] App exited; stopping the watcher.");
      process.exitCode = event.exitCode;
    }
    break;
  }

  const nextSignature = await sourceSignature();
  if (nextSignature === signature) continue;

  await Bun.sleep(150);
  signature = await sourceSignature();
  if (!(await build()) || stopping) continue;

  if (currentApp.exitCode !== null) {
    app = undefined;
    console.log("[waku-dev] App exited; stopping the watcher.");
    process.exitCode = currentApp.exitCode;
    break;
  }

  await stopApp();
  if (!stopping) app = launchApp();
}
