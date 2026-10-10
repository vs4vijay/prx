// Dev preview of the settings UI in a normal browser, with a fake daemon:
//
//   node menubar/settings-ui-tests/preview-server.js [port]
//   open http://127.0.0.1:4173/            (?offline=1 | ?empty=1 | ?broken=1)
//
// Serves ../settings-ui as-is and injects preview-mock.js (a stand-in for
// wry's window.ipc and the Rust bridge) ahead of bridge.js. Never shipped:
// the app embeds only settings-ui/.
"use strict";
const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");

const root = path.join(__dirname, "..", "settings-ui");
const port = Number(process.argv[2] || 4173);
const types = { ".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".css": "text/css; charset=utf-8" };

http.createServer(function (req, res) {
  const url = new URL(req.url, "http://localhost");
  let file = url.pathname === "/" ? "index.html" : url.pathname.slice(1);
  let full = file === "__mock.js" ? path.join(__dirname, "preview-mock.js") : path.join(root, path.basename(file));
  fs.readFile(full, function (error, bytes) {
    if (error) { res.writeHead(404); res.end("not found"); return; }
    let body = bytes;
    if (file === "index.html") {
      body = String(bytes).replace('<script src="bridge.js">', '<script src="__mock.js"></script>\n  <script src="bridge.js">');
    }
    res.writeHead(200, { "content-type": types[path.extname(full)] || "application/octet-stream", "cache-control": "no-store" });
    res.end(body);
  });
}).listen(port, "127.0.0.1", function () {
  console.log("prx settings preview: http://127.0.0.1:" + port + "/");
});
