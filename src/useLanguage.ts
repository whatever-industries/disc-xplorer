import { useEffect, useSyncExternalStore } from "react";
import { currentLanguage, getLanguagePreference, subscribeLanguage } from "./i18n";

export function useLanguage(): string {
  // Include the resolved locale so an OS language change rerenders System default too.
  const snapshot = useSyncExternalStore(subscribeLanguage, () => `${getLanguagePreference()}:${currentLanguage()}`);
  useEffect(() => {
    const locale = currentLanguage();
    document.documentElement.lang = locale;
    document.documentElement.dir = locale === "ar" ? "rtl" : "ltr";
  }, [snapshot]);
  return getLanguagePreference();
}
