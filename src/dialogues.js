/*
 * Boîte de confirmation native.
 *
 * Le greffon `dialog` de Tauri remplace `window.confirm` par une fonction
 * asynchrone qui appelle une commande `confirm`, absente de sa version 2.7 :
 * la promesse échouait sans rien afficher, et un `confirm()` non attendu
 * valait toujours « oui ». On la remplace par la boîte OK / Annuler du greffon
 * (commande `message`). Tout appel doit donc être attendu : `await confirm(…)`.
 * En cas d'échec, la réponse est « non » : rien d'irréversible ne part sans accord.
 */
window.confirm = async function (message) {
  try {
    return await window.__TAURI__.dialog.confirm(String(message), {
      title: "Gastinel Weather Studio",
      kind: "warning",
      okLabel: "Confirmer",
      cancelLabel: "Annuler",
    });
  } catch (e) {
    console.error("Confirmation impossible :", e);
    return false;
  }
};
