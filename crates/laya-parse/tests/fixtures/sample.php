<?php

namespace App\Store;

const VERSION = "1.0";

/** A key store. */
class KeyStore
{
    private array $map = [];

    public function get(string $key): string
    {
        return $this->map[$key] ?? "";
    }

    public function set(string $key, string $value): void
    {
        $this->map[$key] = $value;
    }
}

trait Loggable
{
    public function log(string $msg): void
    {
        echo $msg;
    }
}

function helper(int $x): int
{
    return $x * 2;
}
