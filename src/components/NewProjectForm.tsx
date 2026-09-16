// "New project": a folder, handed to the host.
//
// It creates nothing. There is no database row here and no project object minted in React — the
// form validates that the field is non-empty and passes the path to the host, and a project appears
// in the list when DISCOVERY finds engine sessions under it. That ordering is the contract's
// (§5.6), and it is why the button says "Open project" rather than "Create".
//
// The native picker is the source of truth for a real path; the text field is an entry convenience
// for the case where the owner already knows where they are going.

import { useState } from "react";

export interface NewProjectFormProps {
  initialPath: string;
  busy: boolean;
  onCancel(): void;
  onOpenProject(cwd: string): void;
  /** Opens the host's folder picker and resolves to the chosen path, or `null` if dismissed. */
  onPick?(): Promise<string | null>;
}

export function NewProjectForm({
  initialPath,
  busy,
  onCancel,
  onOpenProject,
  onPick,
}: NewProjectFormProps) {
  const [path, setPath] = useState(initialPath);
  const [error, setError] = useState<string | null>(null);

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    const trimmed = path.trim();
    if (!trimmed) {
      setError("Enter a folder path, or pick one.");
      return;
    }
    setError(null);
    // Whatever is typed here still goes through the host's own validation; this check only keeps an
    // obviously empty request from being sent.
    onOpenProject(trimmed);
  };

  return (
    <form className="new-project" onSubmit={submit} data-testid="new-project-form">
      <label htmlFor="project-path">New project folder</label>
      <input
        id="project-path"
        value={path}
        onChange={(e) => setPath(e.currentTarget.value)}
        aria-describedby={error ? "project-path-error" : undefined}
        aria-invalid={error ? true : undefined}
        spellCheck={false}
      />
      {error && (
        <p className="form-error" id="project-path-error" role="alert">
          {error}
        </p>
      )}
      <div className="row">
        {onPick && (
          <button
            type="button"
            className="icon-btn"
            onClick={() => {
              void onPick().then((picked) => {
                if (picked) {
                  setPath(picked);
                  setError(null);
                }
              });
            }}
          >
            Browse…
          </button>
        )}
        <button type="button" className="icon-btn" onClick={onCancel}>
          Cancel
        </button>
        <button type="submit" className="action" disabled={busy}>
          Open project
        </button>
      </div>
    </form>
  );
}
