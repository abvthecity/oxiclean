import en from "./en.json" with { type: "json" };

export const locales = [en];

export default {
  pt: { hello: "olá", bye: "tchau", yes: "sim", no: "não", thanks: "obrigado" },
  fr: { hello: "bonjour", bye: "au revoir", yes: "oui", no: "non", thanks: "merci" },
  de: { hello: "hallo", bye: "tschüss", yes: "ja", no: "nein", thanks: "danke" },
  es: { hello: "hola", bye: "adiós", yes: "sí", no: "no", thanks: "gracias" },
  it: { hello: "ciao", bye: "arrivederci", yes: "sì", no: "no", thanks: "grazie" },
};
