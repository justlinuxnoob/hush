import React from "react";
import ReactDOM from "react-dom/client";

import App from "./App";
import "./styles.css";

async function start() {
  // A demo build swaps the Rust backend for a fake one so the interface can be
  // worked on without a Google account. `import.meta.env` is replaced at build
  // time, so a normal build drops this branch and never bundles the mock.
  if (import.meta.env.VITE_HUSH_DEMO) {
    const { installMockBackend } = await import("./devmock");
    installMockBackend();
  }

  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>
  );
}

void start();
