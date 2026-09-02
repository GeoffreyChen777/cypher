"use strict";
/**
 * Cypher-spawned `pi` cannot use the macOS login keychain (errSecInteractionNotAllowed).
 * Persist MCP OAuth entries under ~/.pi/agent/mcp-oauth/<account>/tokens.json and
 * serve them back through @napi-rs/keyring so pi-mcp-adapter sees the same API.
 */
const fs = require("node:fs");
const path = require("node:path");
const Module = require("node:module");

function homeDir() {
  return process.env.HOME || process.env.USERPROFILE || "";
}

function entryPath(account) {
  return path.join(homeDir(), ".pi", "agent", "mcp-oauth", account, "tokens.json");
}

function dumpWrite(service, account, password) {
  const dest = process.env.CYPHER_MCP_AUTH_DUMP;
  if (!dest || typeof password !== "string") return;
  try {
    fs.mkdirSync(path.dirname(dest), { recursive: true });
    fs.appendFileSync(
      dest,
      `${JSON.stringify({ service, account, password })}\n`,
      { encoding: "utf8", mode: 0o600 },
    );
  } catch {
    // File store below is the source of truth.
  }
}

function writeFileStore(account, password) {
  if (typeof account !== "string" || typeof password !== "string") return;
  const file = entryPath(account);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, password, { encoding: "utf8", mode: 0o600 });
}

function readFileStore(account) {
  try {
    const text = fs.readFileSync(entryPath(account), "utf8");
    return text || null;
  } catch {
    return null;
  }
}

function wrapEntry(Orig) {
  return class Entry {
    constructor(service, account) {
      this.service = service;
      this.account = account;
      try {
        this.inner = new Orig(service, account);
      } catch {
        this.inner = null;
      }
    }
    setPassword(password) {
      dumpWrite(this.service, this.account, password);
      writeFileStore(this.account, password);
      if (this.inner) {
        try {
          this.inner.setPassword(password);
        } catch {
          // File store is enough for Cypher-spawned Pi.
        }
      }
    }
    getPassword() {
      try {
        if (this.inner) {
          const value = this.inner.getPassword();
          if (typeof value === "string" && value.length > 0) return value;
        }
      } catch {
        // Native keyring often returns Undefined in this process.
      }
      return readFileStore(this.account);
    }
    deleteCredential() {
      try {
        fs.rmSync(path.dirname(entryPath(this.account)), { recursive: true, force: true });
      } catch {
        // ignore
      }
      try {
        return this.inner ? this.inner.deleteCredential() : false;
      } catch {
        return false;
      }
    }
  };
}

const origLoad = Module._load;
Module._load = function (request, parent, isMain) {
  const loaded = origLoad.apply(this, arguments);
  if (request === "@napi-rs/keyring" && loaded && !loaded.__cypherMcpKeyringWrapped) {
    loaded.Entry = wrapEntry(loaded.Entry);
    loaded.__cypherMcpKeyringWrapped = true;
  }
  return loaded;
};
