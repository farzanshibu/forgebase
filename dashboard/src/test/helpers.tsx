import { render } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import App from "../App";
import { setTokenSource } from "../api";
import { MockApi, makeCollection } from "./mockApi";

export function seeded(opts: { requireAuth?: boolean } = {}) {
  const products = makeCollection("products", [
    { name: "title", type: "text", required: true },
    { name: "price", type: "number" },
    { name: "meta", type: "json" },
  ]);
  const mock = new MockApi({
    ...opts,
    collections: [products],
    records: {
      products: [
        { id: "p1", created: "2026-11-01T00:00:00Z", updated: "2026-11-01T00:00:00Z", _version: 1, title: "Pen", price: 3, meta: { color: "blue" } },
        { id: "p2", created: "2026-11-02T00:00:00Z", updated: "2026-11-02T00:00:00Z", _version: 1, title: "Notebook", price: 12, meta: {} },
      ],
    },
  });
  return mock.install();
}

export function renderStudio(path: string) {
  setTokenSource(() => null);
  return render(
    <MemoryRouter initialEntries={[path]}>
      <App />
    </MemoryRouter>,
  );
}
