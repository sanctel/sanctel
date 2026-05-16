import Sidebar from "./core/components/Sidebar";
import ContentArea from "./core/components/ContentArea";
import "./styles/app.css";

export default function App() {
  return (
    <div className="app">
      <Sidebar />
      <ContentArea />
    </div>
  );
}
