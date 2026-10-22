import { useEffect, useState } from "react";

export const ACCENTS = [
  { id: "sky", name: "Sky", value: "#5b9dff" },
  { id: "teal", name: "Teal", value: "#2fc9a6" },
  { id: "violet", name: "Violet", value: "#a78bfa" },
  { id: "amber", name: "Amber", value: "#f2b14b" },
  { id: "rose", name: "Rose", value: "#f472a6" },
  { id: "mint", name: "Mint", value: "#3ddc97" },
] as const;

export type Theme = "dark" | "light";

function read<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    return fallback;
  }
}

function write(key: string, value: string) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* storage can be unavailable in private webviews */
  }
}

export function useAppearance() {
  const query = new URLSearchParams(window.location.search);
  const queryTheme = query.get("theme");
  const queryAccent = query.get("accent");

  const [theme, setThemeState] = useState<Theme>(() =>
    queryTheme === "light" || queryTheme === "dark" ? queryTheme : read<Theme>("nimbus.theme", "dark"),
  );
  const [accent, setAccentState] = useState<string>(() => queryAccent ?? read<string>("nimbus.accent", ACCENTS[0].value));

  useEffect(() => {
    const root = document.documentElement;
    root.dataset.theme = theme;
    root.style.colorScheme = theme;
    root.style.setProperty("--accent", accent);
  }, [theme, accent]);

  const setTheme = (next: Theme) => {
    setThemeState(next);
    write("nimbus.theme", next);
  };

  const setAccent = (next: string) => {
    setAccentState(next);
    write("nimbus.accent", next);
  };

  return {
    theme,
    setTheme,
    accent,
    setAccent,
    toggleTheme: () => setTheme(theme === "dark" ? "light" : "dark"),
  };
}

export type Appearance = ReturnType<typeof useAppearance>;
