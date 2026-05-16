import Sidebar from "./components/Sidebar";
import ContentArea from "./components/ContentArea";
import "./styles/app.css";

export default function App() {
  return (
    <div className="app">
      <Sidebar />
      <ContentArea />
    </div>
  );
}
