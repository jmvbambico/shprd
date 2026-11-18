// The page's entry point.

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App";
import "@xterm/xterm/css/xterm.css";
import "./styles.css";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("index.html has no #root element");
}

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
