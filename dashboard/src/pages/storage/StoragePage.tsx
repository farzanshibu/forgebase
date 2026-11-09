import { Route, Routes } from "react-router-dom";
import BucketList from "./BucketList";
import BucketView from "./BucketView";

export default function StoragePage() {
  return (
    <div>
      <h1 className="text-2xl font-semibold tracking-tight">Object Storage Studio</h1>
      <div className="mt-6">
        <Routes>
          <Route index element={<BucketList />} />
          <Route path="b/:bucket/*" element={<BucketView />} />
          <Route path="*" element={<p className="text-zinc-400">No such page in Object Storage Studio.</p>} />
        </Routes>
      </div>
    </div>
  );
}
