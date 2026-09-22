import { readFile } from "fs";

export interface Shape {
  area(): number;
}

export type Id = string;

export const MAX_SIZE = 100;

/** A circle. */
export class Circle implements Shape {
  constructor(private r: number) {}

  area(): number {
    return Math.PI * this.r * this.r;
  }

  scale(k: number): Circle {
    const next = this.r * k;
    return new Circle(next);
  }
}

export function makeCircle(r: number): Circle {
  return new Circle(r);
}

export const double = (x: number): number => x * 2;

enum Color {
  Red,
  Green,
}
