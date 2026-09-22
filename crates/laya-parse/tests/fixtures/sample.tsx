import React from "react";

type Props = { name: string };

/** Greets someone. */
export function Greeting({ name }: Props) {
  const label = `Hello, ${name}`;
  return (
    <div className="greeting">
      <span>{label}</span>
    </div>
  );
}

export class Counter extends React.Component<Props> {
  state = { n: 0 };

  increment() {
    this.setState({ n: this.state.n + 1 });
  }

  render() {
    return <button onClick={() => this.increment()}>{this.state.n}</button>;
  }
}

export const App = () => <Greeting name="world" />;
