import { listAdmins } from "../../api";
import { ErrorView, Loading } from "../../components/ui";
import { useLoad } from "../../hooks";
import { THEAD, Th, fmtTime } from "./common";

export default function Admins() {
  const { data, error, loading } = useLoad(listAdmins, "admins");
  return (
    <div>
      <ErrorView error={error} />
      {loading && !error && <Loading what="Loading administrators" />}
      {data?.length === 0 && <p className="text-sm text-zinc-400">No administrators.</p>}
      {!!data?.length && (
        <table className="w-full text-sm">
          <thead className={THEAD}>
            <tr><Th>Email</Th><Th>Created</Th><Th>Last sign-in</Th></tr>
          </thead>
          <tbody>
            {data.map((a) => (
              <tr key={a.id} className="border-t border-zinc-800">
                <td className="py-2 pr-3">{a.email}</td>
                <td className="pr-3">{fmtTime(a.created_at)}</td>
                <td className="pr-3">{fmtTime(a.last_login_at)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
