import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { MemoryRouter, useLocation } from "react-router-dom";
import { ApiError } from "../api";
import { fieldErrors } from "../errors";
import { useCreateParam } from "../hooks";
import { toaster } from "../toaster";
import { validate } from "../validate";
import { Button } from "./Button";
import { CreateSheet } from "./CreateSheet";
import { FormField } from "./FormField";

function Where() {
  const l = useLocation();
  return <output aria-label="Path">{l.pathname + l.search}</output>;
}

function NewBucket({ create }: { create: (name: string) => Promise<unknown> }) {
  const [open, setOpen] = useCreateParam();
  const [name, setName] = useState("");
  const [err, setErr] = useState<unknown>();
  return (
    <>
      <Button tone="primary" onClick={() => setOpen(true)}>
        New bucket
      </Button>
      <CreateSheet
        open={open}
        onOpenChange={setOpen}
        title="New bucket"
        submitLabel="Create bucket"
        canSubmit={!validate.bucketName(name)}
        onError={setErr}
        onSubmit={async () => {
          await create(name);
          return { what: `Bucket ${name}`, href: `/storage/b/${name}` };
        }}
      >
        <FormField label="Name" error={(name && validate.bucketName(name)) || fieldErrors(err).name}>
          {(p) => <input {...p} value={name} onChange={(e) => setName(e.target.value)} />}
        </FormField>
      </CreateSheet>
      <Where />
    </>
  );
}

describe("CreateSheet", () => {
  it("opens from ?new=1, validates inline, and on success closes, toasts and opens the new resource", async () => {
    const add = vi.spyOn(toaster, "add");
    const create = vi.fn(async () => ({}));
    render(
      <MemoryRouter initialEntries={["/storage?new=1"]}>
        <NewBucket create={create} />
      </MemoryRouter>,
    );
    const form = await screen.findByRole("form", { name: "New bucket" });
    const submit = screen.getByRole("button", { name: "Create bucket" });
    expect(submit).toBeDisabled();
    await userEvent.type(screen.getByLabelText("Name"), "Bad");
    expect(form).toHaveTextContent(/lower-case/);
    await userEvent.clear(screen.getByLabelText("Name"));
    await userEvent.type(screen.getByLabelText("Name"), "avatars");
    await userEvent.click(submit);
    expect(create).toHaveBeenCalledWith("avatars");
    await waitFor(() => expect(screen.getByLabelText("Path")).toHaveTextContent("/storage/b/avatars"));
    expect(add).toHaveBeenCalledWith(expect.objectContaining({ title: "Bucket avatars created", type: "success" }));
    await waitFor(() => expect(screen.queryByRole("form", { name: "New bucket" })).not.toBeInTheDocument());
  });

  it("stays open on failure and puts field errors next to their field", async () => {
    const create = vi.fn(async () => {
      throw new ApiError({ status: 400, code: "validation_failed", message: "validation failed", requestId: "r9", body: { error: { code: "validation_failed", message: "validation failed", request_id: "r9", details: { fields: { name: { code: "taken", message: "is already used" } } } } } });
    });
    render(
      <MemoryRouter initialEntries={["/storage"]}>
        <NewBucket create={create} />
      </MemoryRouter>,
    );
    await userEvent.click(screen.getByRole("button", { name: "New bucket" }));
    await userEvent.type(await screen.findByLabelText("Name"), "avatars");
    await userEvent.click(screen.getByRole("button", { name: "Create bucket" }));
    expect(await screen.findByText("Is already used")).toBeInTheDocument();
    expect(screen.getByLabelText("Name")).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByRole("alert")).toHaveTextContent("Some fields need fixing");
    expect(screen.queryByRole("list", { name: "Fields to fix" })).not.toBeInTheDocument();
    expect(screen.getByRole("form", { name: "New bucket" })).toBeInTheDocument();
  });
});
