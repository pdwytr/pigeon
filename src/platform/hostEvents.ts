// Subscribe to a host event with a SYNCHRONOUS unsubscribe — copied from demo-studio's
// `app/src/platform/hostEvents.ts`, where it was written to close issue #6.
//
// `@tauri-apps/api`'s `listen()` is async; a React effect's cleanup is not. So a caller can
// unsubscribe while `listen()` is still in flight, and the subscription has to be released the
// moment it resolves. That release is where two facts about Tauri meet:
//
//   * The host registers the JS-side listener entry by EVALUATING a script into the webview
//     (`Webview::listen_js`, tauri 2.11), separately from the command reply that carries the id.
//     On this edge the reply can be handled before that eval has landed.
//   * `_unlisten` first calls the host's `unregisterListener(event, eventId)`, whose body reads
//     `listeners[eventId].handlerId` with no guard. An id that is not registered YET, or not
//     registered ANY MORE, is a `TypeError` thrown inside Tauri — synchronously, inside an async
//     function, so it arrives as a rejected promise.
//
// React StrictMode runs every effect's cleanup right after its first run in dev, so every boot took
// that path and logged an unhandled rejection (the Mac, 2026-09-11, twice before noon). Three rules
// close it: the cancelled-path release waits one macrotask so the registration has landed; every
// release runs through `releaseQuietly`, so a Tauri-internal throw is not an unhandled rejection;
// and `stop` is idempotent, so a second call cannot unlisten an id twice.

type Unlisten = () => unknown;

export interface HostSubscription {
  /** Unsubscribe. Synchronous, idempotent, and safe to call before `listen()` has resolved. */
  stop: () => void;
  /** `true` once the listener is live; `false` if `stop()` came first or the host refused.
   *  Never rejects — a subscriber that cannot learn anything must not take its caller down. */
  registered: Promise<boolean>;
}

// Imported ONCE and shared: two dynamic imports of one module in flight at once resolve unreliably
// under vitest's module mocker, and the console view subscribes to two events back to back.
let eventApi: Promise<typeof import("@tauri-apps/api/event")> | null = null;

function api() {
  if (!eventApi) eventApi = import("@tauri-apps/api/event");
  return eventApi;
}

/**
 * The general form: `start` performs whatever async registration the site needs and resolves to its
 * unlisten. It is handed `live()`, which answers `false` once the caller has stopped, so a handler
 * that fires in the window between `stop()` and the release delivers nothing. A `start` that throws
 * is a host that refused: `registered` resolves `false` and nothing propagates.
 */
export function subscribeHostAsync(
  start: (live: () => boolean) => Promise<Unlisten>,
): HostSubscription {
  let cancelled = false;
  let live: Unlisten | null = null;
  const registered = (async () => {
    const un = await start(() => !cancelled);
    if (cancelled) {
      // Not this tick: give the host's registration eval the chance to land first.
      setTimeout(() => releaseQuietly(un), 0);
      return false;
    }
    live = un;
    return true;
  })().catch(() => false);
  return {
    registered,
    stop() {
      cancelled = true;
      const un = live;
      live = null;
      if (un) releaseQuietly(un);
    },
  };
}

/** The common case: one named host event, its payload handed to `cb`. A `null` payload is skipped. */
export function subscribeHostEvent<T>(event: string, cb: (payload: T) => void): HostSubscription {
  return subscribeHostAsync(async (live) => {
    const { listen } = await api();
    return listen<T>(event, (e) => {
      if (live() && e.payload != null) cb(e.payload);
    });
  });
}

/** Run one unlisten and keep whatever it does from escaping. Nothing above a teardown can act on a
 *  listener that was already gone, and the payload callback is guarded by `cancelled` either way. */
function releaseQuietly(un: Unlisten): void {
  try {
    const result = un();
    if (result && typeof (result as PromiseLike<unknown>).then === "function") {
      (result as Promise<unknown>).catch(() => {});
    }
  } catch {
    // Same reason: a release that throws has nothing left to release.
  }
}
