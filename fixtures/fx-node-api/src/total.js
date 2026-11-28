// A service, not a screen: the manifest names express and no UI framework, which is the whole point of
// this fixture — `node` and `web` are different technologies wearing the same language.
export const total = (items) => items.reduce((s, i) => s + i.price * i.qty, 0);
