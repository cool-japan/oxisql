-- TPC-H Query 6 - Forecasting Revenue Change
SELECT
    sum(l_extendedprice * l_discount) as revenue
FROM
    lineitem
WHERE
    l_shipdate >= date('1994-01-01')
    AND l_shipdate < date('1995-01-01')
    AND l_discount BETWEEN 0.05 AND 0.07
    AND l_quantity < 24;
