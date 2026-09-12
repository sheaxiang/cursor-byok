import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

function readJson(relativePath) {
  return JSON.parse(readFileSync(new URL(relativePath, import.meta.url), "utf8"));
}

test("should disable updater artifacts and endpoints in desktop packages", () => {
  const config = readJson("../src-tauri/tauri.conf.json");

  assert.equal(config.bundle.createUpdaterArtifacts ?? false, false);
  assert.equal(config.plugins?.updater, undefined);
});

test("should deny update checks and installation in the main window", () => {
  const capability = readJson("../src-tauri/capabilities/default.json");
  const updatePermissions = capability.permissions.filter((permission) => {
    const identifier = typeof permission === "string" ? permission : permission.identifier;
    return identifier.startsWith("updater:")
      || identifier === "allow-check-portable-update"
      || identifier === "allow-install-portable-update";
  });

  assert.deepEqual(updatePermissions, []);
});

test("should omit the updater plugin from frontend dependencies", () => {
  const manifest = readJson("../package.json");

  assert.equal(manifest.dependencies["@tauri-apps/plugin-updater"], undefined);
});
