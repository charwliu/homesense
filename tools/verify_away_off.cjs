"use strict";
const fs = require("node:fs");
const vm = require("node:vm");
const assert = require("node:assert/strict");
const path = require("node:path");
const app = fs.readFileSync(path.join(__dirname, "../crates/homesensed/src/web/app.js"), "utf8");
const start = app.indexOf("async function refreshAway() {");
const click = app.indexOf("document.getElementById('away-off').addEventListener", start);
assert(start >= 0 && click > start);
function setup(withClick) {
  const elements = new Map();
  const pending = [];
  const context = {awayBusy: false, awayRevision: "1", awayReadEpoch: 0, awayActionOwner: null, sessionEpoch: 1, csrf: "synthetic", observedRevision: "1",
    crypto: {randomUUID: () => "synthetic-away-request"},
    document: {getElementById(id) {
      if (!elements.has(id)) elements.set(id, {hidden: true, disabled: true, textContent: "", addEventListener(_type, handler) {this.handler = handler;}});
      return elements.get(id);
    }},
    request() {return new Promise((resolve, reject) => pending.push({resolve, reject}));},
    async refreshDevices() {},
  };
  vm.createContext(context);
  vm.runInContext(app.slice(start, withClick ? undefined : click) + "\nglobalThis.refreshProbe = refreshAway;", context);
  return {context, elements, pending};
}
(async () => {
  for (const oldFailure of [true, false]) {
    const {context, elements, pending} = setup(false);
    const old = context.refreshProbe();
    const latest = context.refreshProbe();
    pending[1].resolve({available: oldFailure, revision: "1"});
    await latest;
    if (oldFailure) pending[0].reject(Error("obsolete failure"));
    else pending[0].resolve({available: true, revision: "1"});
    await old;
    assert.equal(elements.get("away-control").hidden, !oldFailure);
    assert.equal(elements.get("away-off").disabled, !oldFailure);
  }
  for (const switchSession of [true, false]) {
    const {context, elements, pending} = setup(true);
    const old = elements.get("away-off").handler();
    assert.equal(context.awayBusy, true);
    if (switchSession) context.sessionEpoch++;
    context.awayActionOwner = {}; // Another current operation owns the lock.
    context.awayBusy = true;
    pending[0].reject(Error("obsolete action result"));
    await old;
    assert.equal(context.awayBusy, true);
  }
  console.log("Away reads and action completion preserve the latest permission/session owner: 4 cases passed; no network or hardware");
})().catch(error => {console.error(error); process.exitCode = 1;});
