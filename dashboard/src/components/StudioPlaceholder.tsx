import type { Studio } from "../studios";

/** Honest "not built yet" panel: lists what the studio will cover, shows no data. */
export default function StudioPlaceholder({ studio }: { studio: Studio }) {
  return (
    <div className="max-w-2xl">
      <h1 className="text-2xl font-semibold tracking-tight">{studio.label}</h1>
      <p className="mt-1 text-muted-foreground">{studio.summary}</p>
      <section className="mt-6 rounded-lg border border-dashed border-border p-6" aria-label="Availability">
        <p className="font-medium text-primary">
          {studio.phase !== null ? `Arrives in phase ${studio.phase}` : "Not available yet"}
        </p>
        <p className="mt-1 text-sm text-muted-foreground">Nothing is shown here until the server API behind it exists.</p>
        <p className="mt-4 text-xs uppercase tracking-wide text-muted-foreground">Planned</p>
        <ul className="mt-2 list-disc space-y-1 pl-5 text-sm text-foreground/80">
          {studio.features.map((f) => (
            <li key={f}>{f}</li>
          ))}
        </ul>
      </section>
    </div>
  );
}
