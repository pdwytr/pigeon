// The app, as a function of its host.
//
// `api` is a prop and not a module-level singleton, which is what lets a test render the whole
// dashboard against `createFakeApi()` and lets `npm run dev` render it in a plain browser with no
// Rust behind it at all. `main.tsx` decides which one; nothing below here knows the difference.

import "./styles/feather.css";
import type { FeatherApi } from "./api/types";
import { AppShell } from "./components/AppShell";
import type { TerminalFactory } from "./console/terminalEngine";

export interface AppProps {
  api: FeatherApi;
  terminalFactory?: TerminalFactory;
  pollMs?: number;
}

export default function App({ api, terminalFactory, pollMs }: AppProps) {
  return <AppShell api={api} terminalFactory={terminalFactory} pollMs={pollMs} />;
}
