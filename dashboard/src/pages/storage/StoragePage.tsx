import { Route, Routes } from "react-router-dom";
import BucketList from "./BucketList";
import BucketView from "./BucketView";

export default function StoragePage() {
  return (
    <Routes>
      <Route index element={<BucketList />} />
      <Route path="b/:bucket/*" element={<BucketView />} />
      <Route path="*" element={<p className="text-muted-foreground">No such page in the Storage studio.</p>} />
    </Routes>
  );
}
