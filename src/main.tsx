import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/500.css";
import "@fontsource/jetbrains-mono/700.css";
import "@fontsource/inter/400.css";
import "@fontsource/inter/500.css";
import "@fontsource/inter/600.css";
import "@fontsource-variable/noto-sans-sc";
import "./index.css";
import {
  applyThemeToDOM,
  THEME_CACHE_KEY,
  THEME_SNAPSHOT_CACHE_KEY,
  ThemeProvider,
} from "./context/ThemeContext";
import {
  scheduleChildWindowShellReady,
  signalChildWindowLoadFailed,
  signalChildWindowLoadStarted,
} from "./lib/childWindowLifecycle";
import { DEFAULT_THEME_ID, themes } from "./lib/themes";
import { installWebviewReloadGuard } from "./lib/webviewReloadGuard";

// Apply cached theme synchronously before React renders to avoid flash
try {
  const cachedId = localStorage.getItem(THEME_CACHE_KEY);
  const cachedTheme = cachedId ? themes[cachedId] : null;
  if (cachedTheme) {
    applyThemeToDOM(cachedTheme.colors);
  } else {
    const snapshot = localStorage.getItem(THEME_SNAPSHOT_CACHE_KEY);
    const parsed = snapshot ? JSON.parse(snapshot) : null;
    if (parsed?.id === cachedId && parsed?.colors) {
      applyThemeToDOM(parsed.colors);
    } else {
      applyThemeToDOM(themes[DEFAULT_THEME_ID].colors);
    }
  }
} catch {}

installWebviewReloadGuard();
document.addEventListener("contextmenu", (e) => e.preventDefault());

const params = new URLSearchParams(window.location.search);
const windowType = params.get("window");

if (windowType) {
  void signalChildWindowLoadStarted().catch(() => {});
  // Child window: lightweight provider stack, no full App
  // These entry points are independent and should load in parallel; serial awaits would add an
  // unnecessary chunk round trip to every child-window open.
  const childRoot = ReactDOM.createRoot(document.getElementById("root") as HTMLElement);
  // Commit an inline-background loading shell before loading provider and page chunks. This lets
  // the parent reveal a stable surface without reintroducing the macOS white or empty window.
  childRoot.render(
    <div
      className="flex h-screen w-full items-center justify-center bg-background"
      style={{ backgroundColor: "var(--df-bg, #0d1117)" }}
      aria-busy="true"
    >
      <span className="size-5 animate-spin rounded-full border-2 border-primary border-t-transparent" />
    </div>,
  );
  scheduleChildWindowShellReady();

  try {
    const [
      { ChildAppProvider },
      { default: ChildWindowRouter },
      { default: ErrorBoundary },
      { Toaster },
    ] = await Promise.all([
      import("./context/ChildAppProvider"),
      import("./ChildWindowRouter"),
      import("./components/ErrorBoundary"),
      import("./components/ui/sonner"),
    ]);

    childRoot.render(
      <React.StrictMode>
        <ErrorBoundary>
          <ChildAppProvider>
            <ThemeProvider>
              <ChildWindowRouter windowType={windowType} />
              <Toaster />
            </ThemeProvider>
          </ChildAppProvider>
        </ErrorBoundary>
      </React.StrictMode>,
    );
  } catch {
    void signalChildWindowLoadFailed("bootstrap-import").catch(() => {});
    let errorTitle = "Something went wrong";
    let reloadLabel = "Reload";
    try {
      const { default: i18n } = await import("./i18n");
      errorTitle = i18n.t("error.somethingWentWrong");
      reloadLabel = i18n.t("error.reloadApplication");
    } catch {}
    childRoot.render(
      <div
        className="flex h-screen w-full items-center justify-center bg-background p-8"
        style={{ backgroundColor: "var(--df-bg, #0d1117)" }}
        role="alert"
      >
        <div className="max-w-md text-center">
          <p className="text-destructive">{errorTitle}</p>
          <button
            type="button"
            className="mt-6 rounded-md bg-primary px-4 py-2 text-primary-foreground"
            onClick={() => window.location.reload()}
          >
            {reloadLabel}
          </button>
        </div>
      </div>,
    );
  }
} else {
  // Main window: full app with all providers
  const [
    { getCurrentWindow },
    { setOwnerMainWindowLabel },
    { AppProvider },
    { default: App },
    { default: ErrorBoundary },
    { McpApprovalDialog },
    { Toaster },
  ] = await Promise.all([
    import("@tauri-apps/api/window"),
    import("./lib/windowManager"),
    import("./context/AppProvider"),
    import("./App"),
    import("./components/ErrorBoundary"),
    import("./components/dialog/ai/McpApprovalDialog"),
    import("./components/ui/sonner"),
  ]);
  setOwnerMainWindowLabel(getCurrentWindow().label);

  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <ErrorBoundary>
        <AppProvider>
          <ThemeProvider>
            <App />
            <McpApprovalDialog />
            <Toaster />
          </ThemeProvider>
        </AppProvider>
      </ErrorBoundary>
    </React.StrictMode>,
  );
}
