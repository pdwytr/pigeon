// Mount the single hover surface. Pigeon no longer boots a dashboard window.
//
// Tauri v2 stamps `__TAURI_INTERNALS__` onto the window before any app script runs, so the check is
// reliable on the first line. With a host, commands go over `invoke`; without one — `npm run dev`
// in an ordinary browser — the in-memory fake serves the same interface for local development.

import React from "react";
import ReactDOM from "react-dom/client";
import { createTauriApi, isTauri } from "./api/client";
import { createFakeApi } from "./api/fake";
import type { FeatherApi } from "./api/types";
import HoverSurface from "./HoverSurface";

// `animate: true` only in the browser: the fake resolves its pending metric a beat after startup
// and echoes keystrokes, so the dev surface behaves like something live rather than a still.
const api: FeatherApi = isTauri() ? createTauriApi() : createFakeApi({ animate: true });

const root = document.getElementById("root");
if (!root) throw new Error("index.html has no #root to mount into");

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    <HoverSurface api={api} pollMs={isTauri() ? 5000 : 0} />
  </React.StrictMode>,
);
