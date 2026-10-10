// JS side of the settings bridge (menubar/src/settings/bridge.rs documents
// the wire format). Each call gets a fresh id; the host echoes it back in
// window.prxBridge.__resolve(reply), so replies may arrive in any order.
// Errors reject with an Error carrying `kind` (bad_request | offline |
// rejected | daemon) and `field` (the offending config path, or null).
(function () {
  "use strict";
  const pending = new Map();
  let nextId = 1;

  function call(method, params) {
    return new Promise(function (resolve, reject) {
      const id = nextId++;
      pending.set(id, { resolve: resolve, reject: reject });
      window.ipc.postMessage(JSON.stringify({ id: id, method: method, params: params || {} }));
    });
  }

  function __resolve(reply) {
    const waiter = pending.get(reply.id);
    if (!waiter) return;
    pending.delete(reply.id);
    if (reply.ok) {
      waiter.resolve(reply.result);
    } else {
      const info = reply.error || { kind: "daemon", message: "unknown error" };
      const error = new Error(info.message);
      error.kind = info.kind;
      error.field = info.field || null;
      waiter.reject(error);
    }
  }

  window.prxBridge = Object.freeze({
    __resolve: __resolve,
    call: call,
    // GET /config -> { path, yaml, raw, effective, config_error? }
    getConfig: function () { return call("config.get"); },
    // PUT /config (structured, the config's own JSON shape)
    putConfig: function (config) { return call("config.put", { config: config }); },
    // PUT /config (raw YAML text, written verbatim when valid)
    putConfigYaml: function (yaml) { return call("config.put", { yaml: yaml }); },
    // POST /review-mode: "off" | "monitor" | "review comment"
    setReviewMode: function (mode) { return call("review_mode.set", { mode: mode }); },
  });
})();
