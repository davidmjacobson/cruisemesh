import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { FluentProvider, webDarkTheme, webLightTheme } from "@fluentui/react-components";
import { App } from "./App";
import "./styles.css";

function Root() {
  const dark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  return (
    <FluentProvider theme={dark ? webDarkTheme : webLightTheme} className="provider">
      <App />
    </FluentProvider>
  );
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Root />
  </StrictMode>,
);
