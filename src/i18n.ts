import en from "./locales/en.json";
import es from "./locales/es.json";
import fr from "./locales/fr.json";
import de from "./locales/de.json";
import it from "./locales/it.json";
import pl from "./locales/pl.json";
import ru from "./locales/ru.json";
import uk from "./locales/uk.json";
import pt from "./locales/pt-BR.json";
import zh from "./locales/zh-CN.json";
import ja from "./locales/ja.json";
import ko from "./locales/ko.json";
import hi from "./locales/hi.json";
import ar from "./locales/ar.json";
import messages from "./locales/messages.json";

export const LANGUAGES = [
  ["en", "English"], ["es", "Español"], ["fr", "Français"], ["de", "Deutsch"], ["it", "Italiano"],
  ["pl", "Polski"], ["ru", "Русский"], ["uk", "Українська"],
  ["pt-BR", "Português (Brasil)"], ["zh-CN", "简体中文"], ["ja", "日本語"],
  ["ko", "한국어"], ["hi", "हिन्दी"], ["ar", "العربية"],
] as const;
export type Language = typeof LANGUAGES[number][0];
export const LANGUAGE_KEY = "interfaceLanguage";
const catalogs: Record<Language, Record<string, string>> = { en, es, fr, de, it, pl, ru, uk, "pt-BR": pt, "zh-CN": zh, ja, ko, hi, ar };
const listeners = new Set<() => void>();
let preference = "system";
try { preference = localStorage.getItem(LANGUAGE_KEY) || "system"; } catch { /* Private/blocked storage. */ }

export function resolveLanguage(value: string, systemLanguages: readonly string[] = []): Language {
  const candidates = value === "system" ? systemLanguages : [value];
  for (const candidate of candidates) {
    const tag = candidate.toLowerCase().replace(/_/g, "-");
    if (tag.startsWith("pt")) return "pt-BR";
    // Do not silently present Simplified Chinese to users requesting Traditional Chinese.
    if (tag === "zh" || tag === "zh-cn" || tag === "zh-sg" || tag.startsWith("zh-hans")) return "zh-CN";
    const base = tag.split("-")[0];
    if (base !== "zh" && Object.prototype.hasOwnProperty.call(catalogs, base)) return base as Language;
  }
  return "en";
}
export function currentLanguage(): Language {
  return resolveLanguage(getLanguagePreference(), typeof navigator === "undefined" ? [] : navigator.languages);
}
export function getLanguagePreference(): string {
  return preference === "system" || LANGUAGES.some(([code]) => code === preference) ? preference : "system";
}
export function setLanguage(value: string): void {
  preference = value === "system" || LANGUAGES.some(([code]) => code === value) ? value : "system";
  try { localStorage.setItem(LANGUAGE_KEY, preference); } catch { /* Keep the in-memory choice. */ }
  listeners.forEach(listener => listener());
}
export function subscribeLanguage(listener: () => void): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}
if (typeof window !== "undefined") {
  window.addEventListener("storage", event => {
    if (event.key !== LANGUAGE_KEY && event.key !== null) return;
    preference = event.newValue || "system";
    listeners.forEach(listener => listener());
  });
  window.addEventListener("languagechange", () => listeners.forEach(listener => listener()));
}

// English phrases are stable catalog keys. Values are never interpreted as markup.
export function translate(language: Language, key: string, ...values: (string | number | null | undefined)[]): string {
  const pattern = catalogs[language][key] || catalogs.en[key] || key;
  return pattern.replace(/\{(\d+)\}/g, (placeholder, index: string) => {
    const value = values[Number(index)];
    if (value === undefined || value === null) return value === null ? "" : placeholder;
    const text = String(value);
    return language === "ar" ? `\u2068${text}\u2069` : text;
  });
}
export function tr(key: string, ...values: (string | number | null | undefined)[]): string {
  return translate(currentLanguage(), key, ...values);
}

// Match only known human-facing messages. Raw logs, command arguments and filenames
// never go through this function; unknown diagnostics remain available verbatim.
const templates = (messages as string[]).filter(key => key.includes("{0}")).map(key => {
  const indices: number[] = [];
  const escaped = key.split(/(\{\d+\})/).map(part => {
    const match = /^\{(\d+)\}$/.exec(part);
    if (match) { indices.push(Number(match[1])); return "([\\s\\S]*?)"; }
    return part.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  }).join("");
  return { key, indices, pattern: new RegExp(`^${escaped}$`) };
});
export function localizeStage(stage: string): string {
  const pass = /^Dump (\d+) · (.+)$/.exec(stage);
  if (pass) return tr("Dump {0} · {1}", pass[1], localizeStage(pass[2]));
  const labels: Record<string, string> = {
    DUMP: "Dumping in Progress", REFINE: "Refining dump", SPLIT: "Generating .bin files",
    HASH: "Calculating hashes", PROTECTION: "Checking protection", INFO: "Analyzing disc",
    SKELETON: "Generating Skeleton", END: "Finishing",
  };
  return tr(labels[stage] || stage);
}
export function localizeMessage(value: string | null | undefined): string {
  if (!value) return "";
  if (Object.prototype.hasOwnProperty.call(en, value)) return tr(value);
  for (const { key, indices, pattern } of templates) {
    const match = pattern.exec(value);
    if (match) {
      const args: string[] = [];
      indices.forEach((index, i) => { args[index] = match[i + 1]; });
      return tr(key, ...args);
    }
  }
  // The backend appends these independent post-processing summaries to its outcome.
  for (const suffix of [" Logs archived.", " Logs archived; some originals kept.", " Hashes match. Both copies kept.", " Hashes differ. Both copies kept."]) {
    if (value.endsWith(suffix)) return `${localizeMessage(value.slice(0, -suffix.length))} ${tr(suffix.trim())}`;
  }
  return value;
}
