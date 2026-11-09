import { Link, NavLink, Route, Routes, useParams } from "react-router-dom";
import { getBucket } from "../../api";
import { ErrorView, Loading } from "../../components/ui";
import { useLoad } from "../../hooks";
import AclEditor from "./AclEditor";
import BucketSettings from "./BucketSettings";
import ObjectBrowser from "./ObjectBrowser";
import SignedUrls from "./SignedUrls";

const tab = ({ isActive }: { isActive: boolean }) => `-mb-px border-b-2 px-3 py-2 text-sm ${isActive ? "border-amber-400 text-white" : "border-transparent text-zinc-400 hover:text-zinc-100"}`;

export default function BucketView() {
  const { bucket = "" } = useParams();
  const b = useLoad((s) => getBucket(bucket, s), `bucket:${bucket}`);
  const base = `/storage/b/${encodeURIComponent(bucket)}`;
  return (
    <div className="space-y-4">
      <div className="flex items-baseline gap-3">
        <Link className="text-sm text-amber-300 hover:underline" to="/storage">All buckets</Link>
        <h2 className="text-xl font-semibold">{bucket}</h2>
        {b.data && <span className="text-sm text-zinc-400">{b.data.visibility} · versioning {b.data.versioning ? "on" : "off"}</span>}
      </div>
      <ErrorView error={b.error} />
      {b.loading && !b.error && <Loading what="Loading bucket" />}
      {b.data && (
        <>
          <nav aria-label="Bucket sections" className="flex gap-1 border-b border-zinc-800">
            <NavLink to={base} end className={tab}>Objects</NavLink>
            <NavLink to={`${base}/settings`} className={tab}>Policy</NavLink>
            <NavLink to={`${base}/access`} className={tab}>Access (ACL)</NavLink>
            <NavLink to={`${base}/share`} className={tab}>Signed URLs</NavLink>
          </nav>
          <Routes>
            <Route index element={<ObjectBrowser bucket={b.data} />} />
            <Route path="settings" element={<BucketSettings bucket={b.data} onSaved={b.reload} />} />
            <Route path="access" element={<AclEditor bucket={b.data} />} />
            <Route path="share" element={<SignedUrls bucket={b.data} />} />
            <Route path="*" element={<p className="text-zinc-400">No such page in this bucket.</p>} />
          </Routes>
        </>
      )}
    </div>
  );
}
